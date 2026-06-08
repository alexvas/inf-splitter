use std::collections::HashSet;
use std::sync::Arc;

use anyllm_translate::anthropic::MessageCreateRequest;
use anyllm_translate::mapping::streaming_map::StreamingTranslator;
use anyllm_translate::openai::ChatCompletionResponse;
use anyllm_translate::{translate_request, translate_response, TranslationConfig};
use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use reqwest::Client as HttpClient;

use crate::auth::forward_request_headers;
use crate::config::{Config, RouteTarget};
use crate::diagnostics::{Diagnostics, DumpEvent, StatsEvent};
use crate::error::AppError;
use crate::relay::cap_openai_max_tokens;
use crate::relay::{DiagnosticStream, RelayContext};
use crate::sse;

#[derive(Clone)]
pub struct OpenAiHandler {
    http: HttpClient,
    diagnostics: Diagnostics,
    hint_statuses: Arc<HashSet<StatusCode>>,
}

impl OpenAiHandler {
    pub fn new(
        config: &Config,
        diagnostics: Diagnostics,
        hint_statuses: Arc<HashSet<StatusCode>>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            http: HttpClient::builder()
                .timeout(config.upstream_timeout)
                .build()
                .map_err(|err| AppError::Internal(err.to_string()))?,
            diagnostics,
            hint_statuses,
        })
    }

    pub async fn handle_from_anthropic(
        &self,
        body: &[u8],
        request_headers: &HeaderMap,
        route: &RouteTarget,
        openai_endpoint: &str,
    ) -> Result<Response, AppError> {
        let body = strip_adaptive_thinking(body);
        let mut req: MessageCreateRequest = serde_json::from_slice(&body).map_err(|err| {
            self.diagnostics.record_stats(&StatsEvent {
                request_id: self.diagnostics.new_request_id(),
                ts: crate::diagnostics::ts_string(),
                direction: "anthropic->openai".into(),
                model: "?".into(),
                upstream: openai_endpoint.into(),
                status: 400,
                duration_ms: 0,
                request_size_bytes: body.len(),
                response_size_bytes: None,
                streaming: false,
                input_messages: None,
                max_tokens: None,
                messages_detail_ingress: None,
                messages_detail_egress: None,
                error: Some(format!("invalid Anthropic body: {err}")),
            });
            AppError::BadRequest(err.to_string())
        })?;
        cap_anthropic_max_tokens(&mut req, route.max_tokens);
        let req = req;

        if req.stream.unwrap_or(false) {
            self.handle_stream_manual(&req, request_headers, route, openai_endpoint)
                .await
        } else {
            self.handle_sync_manual(&req, request_headers, route, openai_endpoint)
                .await
        }
    }

    /// OpenAI ingress → OpenAI upstream (passthrough, no translation).
    pub async fn handle_from_openai(
        &self,
        body: &[u8],
        request_headers: &HeaderMap,
        route: &RouteTarget,
        endpoint: &str,
    ) -> Result<Response, AppError> {
        let request_size = body.len();
        let original_body = body.to_vec();
        // Single JSON parse: extract model, ingress detail, apply caps, then egress detail.
        let mut value: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
        let model = value
            .get("model")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "?".to_string());
        let messages_detail_ingress = crate::diagnostics::messages_detail_from_value(&value);
        crate::apply_token_caps_to_value(&mut value, route);
        let messages_detail_egress = crate::diagnostics::messages_detail_from_value(&value);
        let body = serde_json::to_vec(&value).map_err(|e| AppError::Internal(e.to_string()))?;
        let backend_url = format!("{endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.http
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let downstream_body = if self.diagnostics.dump_enabled() {
            Some(body.clone())
        } else {
            None
        };
        let start = std::time::Instant::now();
        let upstream = builder.body(body).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;
        let is_err = !upstream.status().is_success() && !sse::is_event_stream(upstream.headers());
        if is_err {
            let status = upstream.status();
            let response_headers = upstream.headers().clone();
            let error_body = upstream.text().await.unwrap_or_default();
            let request_id = self.diagnostics.new_request_id();
            self.diagnostics.record_stats(&StatsEvent {
                request_id: request_id.clone(),
                ts: crate::diagnostics::ts_string(),
                direction: "openai->openai".into(),
                model: model.clone(),
                upstream: endpoint.to_string(),
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
            // Ingress dump: original client body before token caps.
            if let Ok(body_str) = String::from_utf8(original_body.clone()) {
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
                        status: None,
                    },
                    true,
                );
            }
            if let Some(ref body_bytes) = downstream_body {
                if let Ok(body_str) = String::from_utf8(body_bytes.to_vec()) {
                    self.diagnostics.record_dump(
                        &crate::diagnostics::DumpEvent {
                            request_id,
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
            }
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut response = Response::builder()
                .status(sc)
                .header(header::CONTENT_TYPE, "application/json");
            for (name, value) in relay_response_headers(&response_headers) {
                response = response.header(name, value);
            }
            return response
                .body(Body::from(error_body))
                .map_err(|err| AppError::Internal(err.to_string()));
        }
        let request_id = self.diagnostics.new_request_id();
        let relayed = relay_openai_upstream(
            upstream,
            Some(RelayContext {
                diagnostics: &self.diagnostics,
                request_id: request_id.clone(),
                model: model.clone(),
            }),
        )
        .await?;
        let is_streaming = sse::is_event_stream(relayed.headers());
        self.diagnostics.record_stats(&StatsEvent {
            request_id: request_id.clone(),
            ts: crate::diagnostics::ts_string(),
            direction: "openai->openai".into(),
            model: model.clone(),
            upstream: endpoint.to_string(),
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
        if let Ok(body_str) = String::from_utf8(original_body) {
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
                    status: None,
                },
                false,
            );
        }
        // Dump the egress request body.
        if let Some(ref body_bytes) = downstream_body {
            if let Ok(body_str) = String::from_utf8(body_bytes.to_vec()) {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id,
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
                    false,
                );
            }
        }
        Ok(relayed)
    }

    fn translation_for(&self, route: &RouteTarget, model: &str) -> TranslationConfig {
        let mut builder = TranslationConfig::builder();
        for mapped in route.model_names.iter().chain([model.to_string()].iter()) {
            builder = builder.model_map(mapped, mapped);
        }
        builder.build()
    }

    async fn handle_sync_manual(
        &self,
        req: &MessageCreateRequest,
        request_headers: &HeaderMap,
        route: &RouteTarget,
        openai_endpoint: &str,
    ) -> Result<Response, AppError> {
        let translation = self.translation_for(route, &req.model);
        let mut openai_req = translate_request(req, &translation)
            .map_err(|err| AppError::Upstream(err.to_string()))?;
        cap_openai_max_tokens(&mut openai_req, route);
        openai_req.stream_options = None;

        let request_size = serde_json::to_vec(req).map(|v| v.len()).unwrap_or(0);
        let messages_detail_ingress = if self.diagnostics.stats_enabled() {
            Some(crate::diagnostics::anthropic_messages_detail(req))
        } else {
            None
        };
        let messages_detail_egress = if self.diagnostics.stats_enabled() {
            Some(crate::diagnostics::openai_messages_detail(&openai_req))
        } else {
            None
        };
        let ingress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(req).ok()
        } else {
            None
        };
        let egress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(&openai_req).ok()
        } else {
            None
        };

        let backend_url = format!("{openai_endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.http
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let start = std::time::Instant::now();
        let upstream = builder.json(&openai_req).send().await?;
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
                direction: "anthropic->openai".into(),
                model: req.model.clone(),
                upstream: openai_endpoint.into(),
                status: status.as_u16(),
                duration_ms,
                request_size_bytes: request_size,
                response_size_bytes: Some(error_body.len()),
                streaming: false,
                input_messages: Some(req.messages.len()),
                max_tokens: Some(req.max_tokens),
                messages_detail_ingress: messages_detail_ingress.clone(),
                messages_detail_egress: messages_detail_egress.clone(),
                error: Some(error_body.clone()),
            });
            // Ingress dump: original Anthropic body.
            if let Some(ref s) = ingress_str {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id: request_id.clone(),
                        ts: crate::diagnostics::ts_string(),
                        stage: "ingress".into(),
                        direction: "request".into(),
                        model: req.model.clone(),
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
            // Egress dump: translated OpenAI body.
            if let Some(ref s) = egress_str {
                self.diagnostics.record_dump(
                    &crate::diagnostics::DumpEvent {
                        request_id,
                        ts: crate::diagnostics::ts_string(),
                        stage: "egress".into(),
                        direction: "request".into(),
                        model: req.model.clone(),
                        headers: Vec::new(),
                        body: s.clone(),
                        status: None,
                    },
                    true,
                );
            }
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = crate::append_size_hint(sc, error_body, &self.hint_statuses);
            return Response::builder()
                .status(sc)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .map_err(|err| AppError::Internal(err.to_string()));
        }

        let openai_resp: ChatCompletionResponse = upstream.json().await?;
        let response = translate_response(&openai_resp, &req.model);

        // Success diagnostics.
        let request_id = self.diagnostics.new_request_id();
        self.diagnostics.record_stats(&StatsEvent {
            request_id: request_id.clone(),
            ts: crate::diagnostics::ts_string(),
            direction: "anthropic->openai".into(),
            model: req.model.clone(),
            upstream: openai_endpoint.into(),
            status: 200,
            duration_ms,
            request_size_bytes: request_size,
            response_size_bytes: None,
            streaming: false,
            input_messages: Some(req.messages.len()),
            max_tokens: Some(req.max_tokens),
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
                    model: req.model.clone(),
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
                    model: req.model.clone(),
                    headers: Vec::new(),
                    body: egress_str,
                    status: None,
                },
                false,
            );
        }
        Ok((StatusCode::OK, axum::Json(response)).into_response())
    }

    async fn handle_stream_manual(
        &self,
        req: &MessageCreateRequest,
        request_headers: &HeaderMap,
        route: &RouteTarget,
        openai_endpoint: &str,
    ) -> Result<Response, AppError> {
        let translation = self.translation_for(route, &req.model);
        let mut openai_req = translate_request(req, &translation)
            .map_err(|err| AppError::Upstream(err.to_string()))?;
        cap_openai_max_tokens(&mut openai_req, route);
        openai_req.stream = Some(true);
        openai_req.stream_options = None;

        let request_size = serde_json::to_vec(req).map(|v| v.len()).unwrap_or(0);
        let messages_detail_ingress = if self.diagnostics.stats_enabled() {
            Some(crate::diagnostics::anthropic_messages_detail(req))
        } else {
            None
        };
        let messages_detail_egress = if self.diagnostics.stats_enabled() {
            Some(crate::diagnostics::openai_messages_detail(&openai_req))
        } else {
            None
        };
        let ingress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(req).ok()
        } else {
            None
        };
        let egress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(&openai_req).ok()
        } else {
            None
        };

        let backend_url = format!("{openai_endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.http
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let start = std::time::Instant::now();
        let upstream = builder.json(&openai_req).send().await?;
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
                direction: "anthropic->openai".into(),
                model: req.model.clone(),
                upstream: openai_endpoint.into(),
                status: status.as_u16(),
                duration_ms,
                request_size_bytes: request_size,
                response_size_bytes: Some(error_body.len()),
                streaming: true,
                input_messages: Some(req.messages.len()),
                max_tokens: Some(req.max_tokens),
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
                        model: req.model.clone(),
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
                        model: req.model.clone(),
                        headers: Vec::new(),
                        body: s.clone(),
                        status: None,
                    },
                    true,
                );
            }
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = crate::append_size_hint(sc, error_body, &self.hint_statuses);
            return Response::builder()
                .status(sc)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .map_err(|err| AppError::Internal(err.to_string()));
        }

        let model = req.model.clone();
        let byte_stream = upstream
            .bytes_stream()
            .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));

        let sse_stream = futures::stream::unfold(
            (byte_stream, StreamingTranslator::new(model), String::new()),
            |(mut byte_stream, mut translator, mut buffer)| async move {
                loop {
                    if let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim_end_matches('\r').to_string();
                        buffer = buffer[line_end + 1..].to_string();
                        if let Some(events) = sse::parse_sse_line(&line, &mut translator) {
                            let payload = events
                                .iter()
                                .map(sse::format_sse_event_str)
                                .collect::<String>();
                            return Some((
                                Ok(bytes::Bytes::from(payload)),
                                (byte_stream, translator, buffer),
                            ));
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
                                    (byte_stream, translator, buffer),
                                ));
                            }
                        }
                        Some(Err(err)) => {
                            return Some((Err(err), (byte_stream, translator, buffer)));
                        }
                        None => {
                            let payload = translator
                                .finish()
                                .iter()
                                .map(sse::format_sse_event_str)
                                .collect::<String>();
                            if payload.is_empty() {
                                return None;
                            }
                            return Some((
                                Ok(bytes::Bytes::from(payload)),
                                (byte_stream, translator, buffer),
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
            direction: "anthropic->openai".into(),
            model: req.model.clone(),
            upstream: openai_endpoint.into(),
            status: 200,
            duration_ms,
            request_size_bytes: request_size,
            response_size_bytes: None,
            streaming: true,
            input_messages: Some(req.messages.len()),
            max_tokens: Some(req.max_tokens),
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
                    model: req.model.clone(),
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
                    model: req.model.clone(),
                    headers: Vec::new(),
                    body: egress_str,
                    status: None,
                },
                false,
            );
        }

        sse::sse_response(request_headers, sse_stream)
    }
}

/// Forward relevant non-hop-by-hop headers from OpenAI upstream responses.
fn relay_response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            let lower = name.to_ascii_lowercase();
            if lower == "content-type"
                || lower.starts_with("x-ratelimit-")
                || lower == "x-request-id"
                || lower == "request-id"
                || lower.starts_with("openai-")
            {
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

async fn relay_openai_upstream(
    upstream: reqwest::Response,
    ctx: Option<RelayContext<'_>>,
) -> Result<Response, AppError> {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let relay_headers = relay_response_headers(&headers);

    if sse::is_event_stream(&headers) {
        let stream = upstream
            .bytes_stream()
            .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));
        if let Some(ctx) = ctx {
            let stream = DiagnosticStream {
                inner: stream,
                buffer: Vec::new(),
                diagnostics: ctx.diagnostics.clone(),
                request_id: ctx.request_id,
                model: ctx.model,
                response_headers: relay_headers.clone(),
                status: status.as_u16(),
                dumped: false,
            };
            let mut response = Response::builder().status(status);
            for (name, value) in &relay_headers {
                response = response.header(name.as_str(), value.as_str());
            }
            if !relay_headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            {
                response = response.header(header::CONTENT_TYPE, "text/event-stream");
            }
            if !relay_headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cache-control"))
            {
                response = response.header(header::CACHE_CONTROL, "no-cache");
            }
            if !relay_headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("connection"))
            {
                response = response.header(header::CONNECTION, "keep-alive");
            }
            return response
                .body(Body::from_stream(stream))
                .map_err(|err| AppError::Internal(err.to_string()));
        }
        let mut response = Response::builder().status(status);
        for (name, value) in &relay_headers {
            response = response.header(name.as_str(), value.as_str());
        }
        if !relay_headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            response = response.header(header::CONTENT_TYPE, "text/event-stream");
        }
        if !relay_headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("cache-control"))
        {
            response = response.header(header::CACHE_CONTROL, "no-cache");
        }
        if !relay_headers
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
    if let Some(ctx) = ctx {
        if let Ok(body_str) = String::from_utf8(body.to_vec()) {
            ctx.diagnostics.record_dump(
                &DumpEvent {
                    request_id: ctx.request_id,
                    ts: crate::diagnostics::ts_string(),
                    stage: "egress".into(),
                    direction: "response".into(),
                    model: ctx.model,
                    headers: relay_headers.clone(),
                    body: body_str,
                    status: Some(status.as_u16()),
                },
                false,
            );
        }
    }
    let mut response = Response::builder().status(status);
    for (name, value) in relay_headers {
        response = response.header(name, value);
    }
    response
        .body(Body::from(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

/// Strip `thinking` field when it has type `adaptive` — not supported by
/// anyllm_translate 0.9.x `ThinkingConfig` enum.  Since Anthropic→OpenAI
/// translation doesn't propagate thinking blocks anyway, removal is safe.
fn strip_adaptive_thinking(body: &[u8]) -> Vec<u8> {
    let mut value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return body.to_vec(),
    };
    if let Some(thinking) = value.get("thinking") {
        if thinking.get("type").and_then(|t| t.as_str()) == Some("adaptive") {
            value.as_object_mut().and_then(|obj| obj.remove("thinking"));
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}

fn cap_anthropic_max_tokens(req: &mut MessageCreateRequest, limit: Option<u32>) {
    let Some(limit) = limit else {
        return;
    };
    if req.max_tokens > limit {
        req.max_tokens = limit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_req(max_tokens: u32) -> MessageCreateRequest {
        serde_json::from_value(serde_json::json!({
            "model": "test",
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .unwrap()
    }

    #[test]
    fn strip_adaptive_thinking_removes_field() {
        let body = br#"{"model":"x","max_tokens":1,"messages":[],"thinking":{"type":"adaptive"}}"#;
        let cleaned = strip_adaptive_thinking(body);
        let v: serde_json::Value = serde_json::from_slice(&cleaned).unwrap();
        assert!(v.get("thinking").is_none());
    }

    #[test]
    fn strip_adaptive_thinking_passes_through_other() {
        let body = br#"{"model":"x","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#;
        let cleaned = strip_adaptive_thinking(body);
        let original: serde_json::Value = serde_json::from_slice(body).unwrap();
        let cleaned: serde_json::Value = serde_json::from_slice(&cleaned).unwrap();
        assert_eq!(cleaned, original);
    }

    #[test]
    fn cap_anthropic_max_tokens_clamps_exceeding() {
        let mut req = make_req(4096);
        cap_anthropic_max_tokens(&mut req, Some(1024));
        assert_eq!(req.max_tokens, 1024);
    }

    #[test]
    fn cap_anthropic_max_tokens_leaves_below_unchanged() {
        let mut req = make_req(512);
        cap_anthropic_max_tokens(&mut req, Some(1024));
        assert_eq!(req.max_tokens, 512);
    }

    #[test]
    fn cap_anthropic_max_tokens_no_limit_unchanged() {
        let mut req = make_req(4096);
        cap_anthropic_max_tokens(&mut req, None);
        assert_eq!(req.max_tokens, 4096);
    }
}
