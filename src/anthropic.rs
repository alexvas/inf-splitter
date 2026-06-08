use std::collections::HashSet;
use std::sync::Arc;

use anyllm_translate::anthropic::{MessageResponse, StreamEvent};
use anyllm_translate::mapping::reverse_streaming_map::ReverseStreamingTranslator;
use anyllm_translate::openai::ChatCompletionRequest;
use anyllm_translate::{
    new_reverse_stream_translator, translate_anthropic_to_openai_response,
    translate_openai_to_anthropic_request, TranslationWarnings,
};
use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use reqwest::RequestBuilder;

use crate::auth::forward_request_headers;
use crate::config::RouteTarget;
use crate::diagnostics::{Diagnostics, StatsEvent};
use crate::error::AppError;
use crate::sse;

#[derive(Clone)]
pub struct AnthropicHandler {
    client: Client,
    diagnostics: Diagnostics,
    hint_statuses: Arc<HashSet<StatusCode>>,
}

impl AnthropicHandler {
    pub fn new(
        config: &crate::config::Config,
        diagnostics: Diagnostics,
        hint_statuses: Arc<HashSet<StatusCode>>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            client: Client::builder()
                .timeout(config.upstream_timeout)
                .build()
                .map_err(|err| AppError::Internal(err.to_string()))?,
            diagnostics,
            hint_statuses,
        })
    }

    /// Anthropic ingress → Anthropic upstream (passthrough).
    pub async fn handle_from_anthropic(
        &self,
        body: Bytes,
        request_headers: &HeaderMap,
        route: &RouteTarget,
        anthropic_endpoint: &str,
    ) -> Result<Response, AppError> {
        let request_size = body.len();
        let original_body = body.clone();
        // Single JSON parse: extract model, ingress detail, apply caps, then egress detail.
        let mut value: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| AppError::BadRequest(e.to_string()))?;
        let model = value
            .get("model")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "?".to_string());
        let messages_detail_ingress = crate::diagnostics::messages_detail_from_value(&value);
        crate::apply_token_caps_to_value(&mut value, route);
        let messages_detail_egress = crate::diagnostics::messages_detail_from_value(&value);
        let body =
            Bytes::from(serde_json::to_vec(&value).map_err(|e| AppError::Internal(e.to_string()))?);
        let egress_body = body.clone();
        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let start = std::time::Instant::now();
        let upstream = builder.body(body).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;
        if !upstream.status().is_success() && !sse::is_event_stream(upstream.headers()) {
            let status = upstream.status();
            let response_headers = copy_response_headers(upstream.headers());
            let error_body = upstream.text().await.unwrap_or_default();
            let request_id = self.diagnostics.new_request_id();
            self.diagnostics.record_stats(&StatsEvent {
                request_id: request_id.clone(),
                ts: crate::diagnostics::ts_string(),
                direction: "anthropic->anthropic".into(),
                model: model.clone(),
                upstream: anthropic_endpoint.into(),
                status: status.as_u16(),
                duration_ms,
                request_size_bytes: request_size,
                response_size_bytes: Some(error_body.len()),
                streaming: false,
                input_messages: None,
                max_tokens: None,
                messages_detail_ingress: messages_detail_ingress.clone(),
                messages_detail_egress: messages_detail_egress.clone(),
                error: Some(error_body.clone()),
            });
            // Egress dump: body sent upstream after token caps.
            if let Ok(body_str) = String::from_utf8(egress_body.to_vec()) {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id: request_id.clone(),
                        ts: crate::diagnostics::ts_string(),
                        stage: "egress".into(),
                        direction: "request".into(),
                        model: model.clone(),
                        headers: request_headers
                            .iter()
                            .filter_map(|(k, v)| {
                                v.to_str()
                                    .ok()
                                    .map(|val| (k.as_str().to_string(), val.to_string()))
                            })
                            .collect(),
                        body: body_str,
                        status: None,
                    },
                    true,
                );
            }
            // Ingress dump: original client body before token caps.
            if let Ok(body_str) = String::from_utf8(original_body.to_vec()) {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id,
                        ts: crate::diagnostics::ts_string(),
                        stage: "ingress".into(),
                        direction: "request".into(),
                        model: model.clone(),
                        headers: request_headers
                            .iter()
                            .filter_map(|(k, v)| {
                                v.to_str()
                                    .ok()
                                    .map(|val| (k.as_str().to_string(), val.to_string()))
                            })
                            .collect(),
                        body: body_str,
                        status: None,
                    },
                    true,
                );
            }
            let sc = axum::http::StatusCode::from_u16(status.as_u16())
                .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
            let mut response = axum::response::Response::builder()
                .status(sc)
                .header(axum::http::header::CONTENT_TYPE, "application/json");
            for (name, value) in response_headers {
                response = response.header(name, value);
            }
            return response
                .body(axum::body::Body::from(error_body))
                .map_err(|err| AppError::Internal(err.to_string()));
        }
        let relayed = relay_upstream_response(upstream).await?;
        let is_streaming = sse::is_event_stream(relayed.headers());
        let request_id = self.diagnostics.new_request_id();
        self.diagnostics.record_stats(&StatsEvent {
            request_id: request_id.clone(),
            ts: crate::diagnostics::ts_string(),
            direction: "anthropic->anthropic".into(),
            model: model.clone(),
            upstream: anthropic_endpoint.into(),
            status: relayed.status().as_u16(),
            duration_ms,
            request_size_bytes: request_size,
            response_size_bytes: None,
            streaming: is_streaming,
            input_messages: None,
            max_tokens: None,
            messages_detail_ingress,
            messages_detail_egress,
            error: None,
        });
        // Ingress dump: original client body before token caps.
        if let Ok(body_str) = String::from_utf8(original_body.to_vec()) {
            self.diagnostics.record_dump(
                &crate::diagnostics::DumpEvent {
                    request_id: request_id.clone(),
                    ts: crate::diagnostics::ts_string(),
                    stage: "ingress".into(),
                    direction: "request".into(),
                    model: model.clone(),
                    headers: request_headers
                        .iter()
                        .filter_map(|(k, v)| {
                            v.to_str()
                                .ok()
                                .map(|val| (k.as_str().to_string(), val.to_string()))
                        })
                        .collect(),
                    body: body_str,
                    status: Some(relayed.status().as_u16()),
                },
                false,
            );
        }
        // Egress dump: body sent upstream after token caps.
        if let Ok(body_str) = String::from_utf8(egress_body.to_vec()) {
            self.diagnostics.record_dump(
                &crate::diagnostics::DumpEvent {
                    request_id,
                    ts: crate::diagnostics::ts_string(),
                    stage: "egress".into(),
                    direction: "request".into(),
                    model,
                    headers: request_headers
                        .iter()
                        .filter_map(|(k, v)| {
                            v.to_str()
                                .ok()
                                .map(|val| (k.as_str().to_string(), val.to_string()))
                        })
                        .collect(),
                    body: body_str,
                    status: Some(relayed.status().as_u16()),
                },
                false,
            );
        }
        Ok(relayed)
    }

    /// OpenAI ingress → Anthropic upstream (translate request/response).
    pub async fn handle_from_openai(
        &self,
        body: &[u8],
        request_headers: &HeaderMap,
        route: &RouteTarget,
        anthropic_endpoint: &str,
    ) -> Result<Response, AppError> {
        let request_size = body.len();
        let mut openai_req: ChatCompletionRequest =
            serde_json::from_slice(body).map_err(|err| {
                self.diagnostics.record_stats(&StatsEvent {
                    request_id: self.diagnostics.new_request_id(),
                    ts: crate::diagnostics::ts_string(),
                    direction: "openai->anthropic".into(),
                    model: "?".into(),
                    upstream: anthropic_endpoint.into(),
                    status: 400,
                    duration_ms: 0,
                    request_size_bytes: body.len(),
                    response_size_bytes: None,
                    streaming: false,
                    input_messages: None,
                    max_tokens: None,
                    messages_detail_ingress: None,
                    messages_detail_egress: None,
                    error: Some(format!("invalid OpenAI body: {err}")),
                });
                AppError::BadRequest(err.to_string())
            })?;
        cap_openai_max_tokens(&mut openai_req, route);

        if openai_req.stream.unwrap_or(false) {
            return self
                .handle_from_openai_stream(
                    &openai_req,
                    request_headers,
                    route,
                    anthropic_endpoint,
                    request_size,
                )
                .await;
        }

        let mut warnings = TranslationWarnings::default();
        let anthropic_req = translate_openai_to_anthropic_request(&openai_req, &mut warnings)
            .map_err(|err| AppError::BadRequest(err.to_string()))?;

        let messages_detail_ingress = if self.diagnostics.stats_enabled() {
            Some(crate::diagnostics::openai_messages_detail(&openai_req))
        } else {
            None
        };
        let messages_detail_egress = if self.diagnostics.stats_enabled() {
            Some(crate::diagnostics::anthropic_messages_detail(
                &anthropic_req,
            ))
        } else {
            None
        };
        let ingress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(&openai_req).ok()
        } else {
            None
        };
        let egress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(&anthropic_req).ok()
        } else {
            None
        };

        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let start = std::time::Instant::now();
        let upstream = builder.json(&anthropic_req).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
            let request_id = self.diagnostics.new_request_id();
            self.diagnostics.record_stats(&StatsEvent {
                request_id: request_id.clone(),
                ts: crate::diagnostics::ts_string(),
                direction: "openai->anthropic".into(),
                model: openai_req.model.clone(),
                upstream: anthropic_endpoint.into(),
                status: status.as_u16(),
                duration_ms,
                request_size_bytes: request_size,
                response_size_bytes: Some(error_body.len()),
                streaming: false,
                input_messages: Some(openai_req.messages.len()),
                max_tokens: openai_req.max_tokens.or(openai_req.max_completion_tokens),
                messages_detail_ingress: messages_detail_ingress.clone(),
                messages_detail_egress: messages_detail_egress.clone(),
                error: Some(error_body.clone()),
            });
            if let Some(ref s) = ingress_str {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id: request_id.clone(),
                        ts: crate::diagnostics::ts_string(),
                        stage: "ingress".into(),
                        direction: "request".into(),
                        model: openai_req.model.clone(),
                        headers: request_headers
                            .iter()
                            .filter_map(|(k, v)| {
                                v.to_str()
                                    .ok()
                                    .map(|val| (k.as_str().to_string(), val.to_string()))
                            })
                            .collect(),
                        body: s.clone(),
                        status: None,
                    },
                    true,
                );
            }
            if let Some(ref s) = egress_str {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id,
                        ts: crate::diagnostics::ts_string(),
                        stage: "egress".into(),
                        direction: "request".into(),
                        model: openai_req.model.clone(),
                        headers: Vec::new(),
                        body: s.clone(),
                        status: None,
                    },
                    true,
                );
            }
            return relay_error_body(status, error_body, &self.hint_statuses);
        }

        let anthropic_resp: MessageResponse = upstream.json().await?;
        let openai_resp =
            translate_anthropic_to_openai_response(&anthropic_resp, &openai_req.model);

        // Success diagnostics.
        let request_id = self.diagnostics.new_request_id();
        self.diagnostics.record_stats(&StatsEvent {
            request_id: request_id.clone(),
            ts: crate::diagnostics::ts_string(),
            direction: "openai->anthropic".into(),
            model: openai_req.model.clone(),
            upstream: anthropic_endpoint.into(),
            status: 200,
            duration_ms,
            request_size_bytes: request_size,
            response_size_bytes: None,
            streaming: false,
            input_messages: Some(openai_req.messages.len()),
            max_tokens: openai_req.max_tokens.or(openai_req.max_completion_tokens),
            messages_detail_ingress,
            messages_detail_egress,
            error: None,
        });
        if let Some(ingress_str) = ingress_str {
            self.diagnostics.record_dump(
                &crate::diagnostics::DumpEvent {
                    request_id: request_id.clone(),
                    ts: crate::diagnostics::ts_string(),
                    stage: "ingress".into(),
                    direction: "request".into(),
                    model: openai_req.model.clone(),
                    headers: request_headers
                        .iter()
                        .filter_map(|(k, v)| {
                            v.to_str()
                                .ok()
                                .map(|val| (k.as_str().to_string(), val.to_string()))
                        })
                        .collect(),
                    body: ingress_str,
                    status: None,
                },
                false,
            );
        }
        if let Some(egress_str) = egress_str {
            self.diagnostics.record_dump(
                &crate::diagnostics::DumpEvent {
                    request_id,
                    ts: crate::diagnostics::ts_string(),
                    stage: "egress".into(),
                    direction: "request".into(),
                    model: openai_req.model,
                    headers: Vec::new(),
                    body: egress_str,
                    status: None,
                },
                false,
            );
        }
        Ok((StatusCode::OK, axum::Json(openai_resp)).into_response())
    }

    async fn handle_from_openai_stream(
        &self,
        openai_req: &ChatCompletionRequest,
        request_headers: &HeaderMap,
        route: &RouteTarget,
        anthropic_endpoint: &str,
        request_size: usize,
    ) -> Result<Response, AppError> {
        let mut warnings = TranslationWarnings::default();
        let mut anthropic_req = translate_openai_to_anthropic_request(openai_req, &mut warnings)
            .map_err(|err| AppError::BadRequest(err.to_string()))?;
        anthropic_req.stream = Some(true);

        let messages_detail_ingress = if self.diagnostics.stats_enabled() {
            Some(crate::diagnostics::openai_messages_detail(openai_req))
        } else {
            None
        };
        let messages_detail_egress = if self.diagnostics.stats_enabled() {
            Some(crate::diagnostics::anthropic_messages_detail(
                &anthropic_req,
            ))
        } else {
            None
        };
        let ingress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(openai_req).ok()
        } else {
            None
        };
        let egress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(&anthropic_req).ok()
        } else {
            None
        };

        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let start = std::time::Instant::now();
        let upstream = builder.json(&anthropic_req).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
            let request_id = self.diagnostics.new_request_id();
            self.diagnostics.record_stats(&StatsEvent {
                request_id: request_id.clone(),
                ts: crate::diagnostics::ts_string(),
                direction: "openai->anthropic".into(),
                model: openai_req.model.clone(),
                upstream: anthropic_endpoint.into(),
                status: status.as_u16(),
                duration_ms,
                request_size_bytes: request_size,
                response_size_bytes: Some(error_body.len()),
                streaming: true,
                input_messages: Some(openai_req.messages.len()),
                max_tokens: openai_req.max_tokens.or(openai_req.max_completion_tokens),
                messages_detail_ingress: messages_detail_ingress.clone(),
                messages_detail_egress: messages_detail_egress.clone(),
                error: Some(error_body.clone()),
            });
            if let Some(ref s) = ingress_str {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id: request_id.clone(),
                        ts: crate::diagnostics::ts_string(),
                        stage: "ingress".into(),
                        direction: "request".into(),
                        model: openai_req.model.clone(),
                        headers: request_headers
                            .iter()
                            .filter_map(|(k, v)| {
                                v.to_str()
                                    .ok()
                                    .map(|val| (k.as_str().to_string(), val.to_string()))
                            })
                            .collect(),
                        body: s.clone(),
                        status: None,
                    },
                    true,
                );
            }
            if let Some(ref s) = egress_str {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id,
                        ts: crate::diagnostics::ts_string(),
                        stage: "egress".into(),
                        direction: "request".into(),
                        model: openai_req.model.clone(),
                        headers: Vec::new(),
                        body: s.clone(),
                        status: None,
                    },
                    true,
                );
            }
            return relay_error_body(status, error_body, &self.hint_statuses);
        }

        let model = openai_req.model.clone();
        let byte_stream = upstream
            .bytes_stream()
            .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));

        let sse_stream = futures::stream::unfold(
            (
                byte_stream,
                None::<ReverseStreamingTranslator>,
                String::new(),
                model,
                false,
            ),
            |(mut byte_stream, mut translator, mut buffer, model, sent_done)| async move {
                if sent_done {
                    return None;
                }

                loop {
                    if let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim_end_matches('\r').to_string();
                        buffer = buffer[line_end + 1..].to_string();
                        if let Some(event) = sse::parse_anthropic_sse_event(&line) {
                            if matches!(event, StreamEvent::MessageStart { .. }) {
                                if let StreamEvent::MessageStart { message } = event {
                                    translator = Some(new_reverse_stream_translator(
                                        message.id,
                                        model.clone(),
                                    ));
                                }
                                continue;
                            }

                            if let Some(ref mut active) = translator {
                                if let Some(chunk) = active.process_event(&event).into_iter().next()
                                {
                                    let payload = sse::format_openai_sse_chunk(&chunk);
                                    return Some((
                                        Ok(bytes::Bytes::from(payload)),
                                        (byte_stream, translator, buffer, model, sent_done),
                                    ));
                                }
                                if active.is_done() {
                                    return Some((
                                        Ok(bytes::Bytes::from("data: [DONE]\n\n")),
                                        (byte_stream, translator, buffer, model, true),
                                    ));
                                }
                            }
                        }
                        continue;
                    }

                    match byte_stream.next().await {
                        Some(Ok(chunk)) => {
                            match String::from_utf8(chunk.to_vec()) {
                                Ok(s) => buffer.push_str(&s),
                                Err(e) => {
                                    tracing::warn!("invalid UTF-8 in SSE stream: {e}");
                                    buffer.push_str(&String::from_utf8_lossy(e.as_bytes()));
                                }
                            }
                            if buffer.len() > sse::MAX_SSE_LINE_LENGTH {
                                return Some((
                                    Err(std::io::Error::other("SSE line too long")),
                                    (byte_stream, translator, buffer, model, sent_done),
                                ));
                            }
                        }
                        Some(Err(err)) => {
                            return Some((
                                Err(err),
                                (byte_stream, translator, buffer, model, sent_done),
                            ));
                        }
                        None => {
                            if sent_done {
                                return None;
                            }
                            return Some((
                                Ok(bytes::Bytes::from("data: [DONE]\n\n")),
                                (byte_stream, translator, buffer, model, true),
                            ));
                        }
                    }
                }
            },
        );

        // Success diagnostics: recorded before returning the stream.
        let request_id = self.diagnostics.new_request_id();
        self.diagnostics.record_stats(&StatsEvent {
            request_id: request_id.clone(),
            ts: crate::diagnostics::ts_string(),
            direction: "openai->anthropic".into(),
            model: openai_req.model.clone(),
            upstream: anthropic_endpoint.into(),
            status: 200,
            duration_ms,
            request_size_bytes: request_size,
            response_size_bytes: None,
            streaming: true,
            input_messages: Some(openai_req.messages.len()),
            max_tokens: openai_req.max_tokens.or(openai_req.max_completion_tokens),
            messages_detail_ingress,
            messages_detail_egress,
            error: None,
        });
        if let Some(ingress_str) = ingress_str {
            self.diagnostics.record_dump(
                &crate::diagnostics::DumpEvent {
                    request_id: request_id.clone(),
                    ts: crate::diagnostics::ts_string(),
                    stage: "ingress".into(),
                    direction: "request".into(),
                    model: openai_req.model.clone(),
                    headers: request_headers
                        .iter()
                        .filter_map(|(k, v)| {
                            v.to_str()
                                .ok()
                                .map(|val| (k.as_str().to_string(), val.to_string()))
                        })
                        .collect(),
                    body: ingress_str,
                    status: None,
                },
                false,
            );
        }
        if let Some(egress_str) = egress_str {
            self.diagnostics.record_dump(
                &crate::diagnostics::DumpEvent {
                    request_id,
                    ts: crate::diagnostics::ts_string(),
                    stage: "egress".into(),
                    direction: "request".into(),
                    model: openai_req.model.clone(),
                    headers: Vec::new(),
                    body: egress_str,
                    status: None,
                },
                false,
            );
        }

        sse::sse_response(request_headers, sse_stream)
    }

    fn build_upstream_request(
        &self,
        request_headers: &HeaderMap,
        route: &RouteTarget,
        anthropic_endpoint: &str,
    ) -> Result<RequestBuilder, AppError> {
        let url = format!("{anthropic_endpoint}/v1/messages");
        Ok(forward_request_headers(
            self.client
                .post(url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        ))
    }
}

fn relay_error_body(
    status: reqwest::StatusCode,
    body: String,
    hint_statuses: &HashSet<StatusCode>,
) -> Result<Response, AppError> {
    let status_code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = crate::append_size_hint(status_code, body, hint_statuses);
    Response::builder()
        .status(status_code)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

async fn relay_upstream_response(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = upstream.status();
    let response_headers = copy_response_headers(upstream.headers());

    if sse::is_event_stream(upstream.headers()) {
        let stream = upstream
            .bytes_stream()
            .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));
        let mut response = Response::builder().status(status);
        for (name, value) in &response_headers {
            response = response.header(name.as_str(), value.as_str());
        }
        if !response_headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            response = response.header(header::CONTENT_TYPE, "text/event-stream");
        }
        if !response_headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("cache-control"))
        {
            response = response.header(header::CACHE_CONTROL, "no-cache");
        }
        if !response_headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("connection"))
        {
            response = response.header(header::CONNECTION, "keep-alive");
        }
        return response
            .body(Body::from_stream(stream))
            .map_err(|err| AppError::Internal(err.to_string()));
    }

    let body = upstream.bytes().await?;
    let mut response = Response::builder().status(status);
    for (name, value) in response_headers {
        response = response.header(name, value);
    }
    response
        .body(Body::from(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

fn copy_response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            if matches!(
                name,
                "content-type"
                    | "request-id"
                    | "anthropic-ratelimit-requests-limit"
                    | "anthropic-ratelimit-requests-remaining"
                    | "anthropic-ratelimit-requests-reset"
                    | "anthropic-ratelimit-tokens-limit"
                    | "anthropic-ratelimit-tokens-remaining"
                    | "anthropic-ratelimit-tokens-reset"
            ) {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.to_string(), v.to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Apply per-route token caps to an OpenAI `ChatCompletionRequest`.
///
/// Only for **OpenAI egress paths**: passthrough to an OpenAI upstream, or
/// Anthropic-to-OpenAI translation. Mutates the request in place, clamping or
/// setting `max_tokens`, `max_completion_tokens`, and `max_output_tokens`
/// (via `extra`) to the route's configured limits.
pub(crate) fn cap_openai_max_tokens(req: &mut ChatCompletionRequest, route: &RouteTarget) {
    if let Some(limit) = route.max_tokens {
        match req.max_tokens {
            Some(existing) if existing > limit => req.max_tokens = Some(limit),
            None => req.max_tokens = Some(limit),
            _ => {}
        }
    }
    if let Some(limit) = route.max_completion_tokens {
        match req.max_completion_tokens {
            Some(existing) if existing > limit => req.max_completion_tokens = Some(limit),
            None => req.max_completion_tokens = Some(limit),
            _ => {}
        }
    }
    if let Some(limit) = route.max_output_tokens {
        match req.extra.get("max_output_tokens").and_then(|v| v.as_u64()) {
            Some(existing) if existing > limit as u64 => {
                req.extra
                    .insert("max_output_tokens".to_string(), serde_json::json!(limit));
            }
            None => {
                req.extra
                    .insert("max_output_tokens".to_string(), serde_json::json!(limit));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn empty_route() -> RouteTarget {
        RouteTarget {
            section: "test".into(),
            endpoint_openai: None,
            endpoint_anthropic: None,
            api_key: None,
            max_tokens: None,
            max_output_tokens: None,
            max_completion_tokens: None,
            model_names: HashSet::new(),
        }
    }

    fn make_openai_req() -> ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .unwrap()
    }

    #[test]
    fn cap_openai_max_tokens_sets_missing() {
        let mut req = make_openai_req();
        let mut route = empty_route();
        route.max_tokens = Some(1024);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_tokens, Some(1024));
    }

    #[test]
    fn cap_openai_max_tokens_clamps_exceeding() {
        let mut req = make_openai_req();
        req.max_tokens = Some(4096);
        let mut route = empty_route();
        route.max_tokens = Some(1024);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_tokens, Some(1024));
    }

    #[test]
    fn cap_openai_max_tokens_leaves_below() {
        let mut req = make_openai_req();
        req.max_tokens = Some(512);
        let mut route = empty_route();
        route.max_tokens = Some(1024);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_tokens, Some(512));
    }

    #[test]
    fn cap_openai_max_completion_tokens_sets_missing() {
        let mut req = make_openai_req();
        let mut route = empty_route();
        route.max_completion_tokens = Some(2048);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_completion_tokens, Some(2048));
    }

    #[test]
    fn cap_openai_max_output_tokens_sets_missing_via_extra() {
        let mut req = make_openai_req();
        let mut route = empty_route();
        route.max_output_tokens = Some(500);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(
            req.extra.get("max_output_tokens").and_then(|v| v.as_u64()),
            Some(500)
        );
    }

    #[test]
    fn cap_openai_max_output_tokens_clamps_exceeding() {
        let mut req = make_openai_req();
        req.extra
            .insert("max_output_tokens".into(), serde_json::json!(1000u64));
        let mut route = empty_route();
        route.max_output_tokens = Some(500);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(
            req.extra.get("max_output_tokens").and_then(|v| v.as_u64()),
            Some(500)
        );
    }

    #[test]
    fn cap_openai_no_limits_leaves_unchanged() {
        let mut req = make_openai_req();
        req.max_tokens = Some(4096);
        let route = empty_route();
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_tokens, Some(4096));
    }

    #[test]
    fn copy_response_headers_filters_to_whitelist() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("request-id", "req-123".parse().unwrap());
        headers.insert("x-custom", "should-be-dropped".parse().unwrap());
        headers.insert(
            "anthropic-ratelimit-requests-limit",
            "1000".parse().unwrap(),
        );
        headers.insert("connection", "keep-alive".parse().unwrap());

        let result = copy_response_headers(&headers);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"content-type"));
        assert!(names.contains(&"request-id"));
        assert!(names.contains(&"anthropic-ratelimit-requests-limit"));
        assert!(!names.contains(&"x-custom"));
        assert!(!names.contains(&"connection"));
    }
}
