use std::collections::HashMap;
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

use crate::auth::{forward_request_headers, forward_request_headers_map};
use crate::config::RouteTarget;
use crate::diagnostics::{Diagnostics, RequestDiagnostics, StatsEvent};
use crate::error::AppError;
use crate::relay::cap_openai_max_tokens;
use crate::relay::DiagnosticStream;
use crate::sse;

#[derive(Clone)]
pub struct AnthropicHandler {
    clients: HashMap<String, Client>,
    diagnostics: Diagnostics,
    error_translation: Arc<[crate::config::ErrorTranslationRule]>,
}

impl AnthropicHandler {
    pub fn new(config: &crate::config::Config, diagnostics: Diagnostics) -> Result<Self, AppError> {
        Ok(Self {
            clients: crate::build_client_map(config)?,
            diagnostics,
            error_translation: config.error_translation.clone().into(),
        })
    }

    fn get_client(&self, proxy: Option<&str>) -> &Client {
        proxy
            .and_then(|url| self.clients.get(url))
            .unwrap_or_else(|| self.clients.get("").expect("default client must exist"))
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
        let mut value: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| AppError::BadRequest(e.to_string()))?;
        let model = value
            .get("model")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "?".to_string());

        let guard = RequestDiagnostics::new(&self.diagnostics, &route.section, &model);
        guard.ingress_dump(&original_body, request_headers);

        if self.diagnostics.stats_enabled() {
            if let Some(detail) = crate::diagnostics::messages_detail_from_value(&value) {
                guard.set_messages_detail_ingress(detail);
            }
        }
        crate::apply_egress_transforms(&mut value, &model, route);
        if self.diagnostics.stats_enabled() {
            if let Some(detail) = crate::diagnostics::messages_detail_from_value(&value) {
                guard.set_messages_detail_egress(detail);
            }
        }
        let body =
            Bytes::from(serde_json::to_vec(&value).map_err(|e| AppError::Internal(e.to_string()))?);
        if self.diagnostics.dump_enabled() {
            let egress_headers =
                forward_request_headers_map(route.api_key.as_deref(), request_headers);
            guard.egress_dump(&body, &egress_headers);
        }
        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let start = std::time::Instant::now();
        let upstream = builder.body(body).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;
        if !upstream.status().is_success() && !sse::is_event_stream(upstream.headers()) {
            let status = upstream.status();
            let response_headers = copy_response_headers(upstream.headers());
            let error_body = upstream.text().await.unwrap_or_default();
            guard.response_dump(
                crate::diagnostics::dump_body_from_bytes(error_body.as_bytes()),
                status.as_u16(),
                true,
                response_headers.clone(),
            );
            guard.finish_with_error(
                status.as_u16(),
                duration_ms,
                request_size,
                Some(error_body.len()),
                anthropic_endpoint,
                "anthropic->anthropic",
                false,
                error_body.clone(),
            );
            let sc = axum::http::StatusCode::from_u16(status.as_u16())
                .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
            let error_body =
                crate::apply_error_translation(sc, error_body, &self.error_translation);
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
        let relayed = relay_upstream_response(upstream, &guard).await?;
        let is_streaming = sse::is_event_stream(relayed.headers());
        guard.finish(
            relayed.status().as_u16(),
            duration_ms,
            request_size,
            None,
            anthropic_endpoint,
            "anthropic->anthropic",
            is_streaming,
        );
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
                    section: route.section.clone(),
                    request_id: self.diagnostics.new_request_id(),
                    ts: crate::diagnostics::ts_string(),
                    direction: "openai->anthropic".into(),
                    model: "?".into(),
                    upstream: anthropic_endpoint.into(),
                    status: 400,
                    duration_ms: 0,
                    request_size_bytes: body.len(),
                    error: Some(format!("invalid OpenAI body: {err}")),
                    ..Default::default()
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

        let prepared = crate::prepare_egress_body(
            &anthropic_req,
            &openai_req.model,
            route,
            &self.diagnostics,
        )?;

        let guard = RequestDiagnostics::new(&self.diagnostics, &route.section, &openai_req.model);
        if self.diagnostics.stats_enabled() {
            guard.set_input_messages(openai_req.messages.len());
            guard.set_max_tokens(
                openai_req
                    .max_tokens
                    .or(openai_req.max_completion_tokens)
                    .unwrap_or(0),
            );
            guard.set_messages_detail_ingress(crate::diagnostics::openai_messages_detail(
                &openai_req,
            ));
            if let Some(detail) = crate::diagnostics::messages_detail_from_value(&prepared.value) {
                guard.set_messages_detail_egress(detail);
            }
        }
        if let Some(ref s) = prepared.egress_str {
            let egress_headers =
                forward_request_headers_map(route.api_key.as_deref(), request_headers);
            guard.egress_dump(s.as_bytes(), &egress_headers);
        }
        let ingress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(&openai_req).ok()
        } else {
            None
        };
        if let Some(ref s) = ingress_str {
            guard.ingress_dump(s.as_bytes(), request_headers);
        }

        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let start = std::time::Instant::now();
        let upstream = builder.body(prepared.bytes).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
            guard.finish_with_error(
                status.as_u16(),
                duration_ms,
                request_size,
                Some(error_body.len()),
                anthropic_endpoint,
                "openai->anthropic",
                false,
                error_body.clone(),
            );
            return relay_error_body(status, error_body, &self.error_translation);
        }

        let anthropic_resp: MessageResponse = upstream.json().await?;
        let openai_resp =
            translate_anthropic_to_openai_response(&anthropic_resp, &openai_req.model);

        guard.finish(
            200,
            duration_ms,
            request_size,
            None,
            anthropic_endpoint,
            "openai->anthropic",
            false,
        );
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

        let prepared = crate::prepare_egress_body(
            &anthropic_req,
            &openai_req.model,
            route,
            &self.diagnostics,
        )?;

        let guard = RequestDiagnostics::new(&self.diagnostics, &route.section, &openai_req.model);
        if self.diagnostics.stats_enabled() {
            guard.set_input_messages(openai_req.messages.len());
            guard.set_max_tokens(
                openai_req
                    .max_tokens
                    .or(openai_req.max_completion_tokens)
                    .unwrap_or(0),
            );
            guard.set_messages_detail_ingress(crate::diagnostics::openai_messages_detail(
                openai_req,
            ));
            if let Some(detail) = crate::diagnostics::messages_detail_from_value(&prepared.value) {
                guard.set_messages_detail_egress(detail);
            }
        }
        if let Some(ref s) = prepared.egress_str {
            let egress_headers =
                forward_request_headers_map(route.api_key.as_deref(), request_headers);
            guard.egress_dump(s.as_bytes(), &egress_headers);
        }
        let ingress_str = if self.diagnostics.dump_enabled() {
            serde_json::to_string(openai_req).ok()
        } else {
            None
        };
        if let Some(ref s) = ingress_str {
            guard.ingress_dump(s.as_bytes(), request_headers);
        }

        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let start = std::time::Instant::now();
        let upstream = builder.body(prepared.bytes).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
            guard.finish_with_error(
                status.as_u16(),
                duration_ms,
                request_size,
                Some(error_body.len()),
                anthropic_endpoint,
                "openai->anthropic",
                true,
                error_body.clone(),
            );
            return relay_error_body(status, error_body, &self.error_translation);
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

        let resp = sse::sse_response(request_headers, sse_stream)?;

        guard.finish(
            200,
            duration_ms,
            request_size,
            None,
            anthropic_endpoint,
            "openai->anthropic",
            true,
        );

        Ok(resp)
    }

    fn build_upstream_request(
        &self,
        request_headers: &HeaderMap,
        route: &RouteTarget,
        anthropic_endpoint: &str,
    ) -> Result<RequestBuilder, AppError> {
        let url = format!("{anthropic_endpoint}/v1/messages");
        Ok(forward_request_headers(
            self.get_client(route.proxy.as_deref())
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
    error_translation: &[crate::config::ErrorTranslationRule],
) -> Result<Response, AppError> {
    let status_code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = crate::apply_error_translation(status_code, body, error_translation);
    Response::builder()
        .status(status_code)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

async fn relay_upstream_response(
    upstream: reqwest::Response,
    guard: &RequestDiagnostics,
) -> Result<Response, AppError> {
    let status = upstream.status();
    let response_headers = copy_response_headers(upstream.headers());

    if sse::is_event_stream(upstream.headers()) {
        let stream = upstream
            .bytes_stream()
            .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));
        let stream = DiagnosticStream {
            inner: stream,
            buffer: Vec::new(),
            diagnostics: guard.diagnostics_handle(),
            request_id: guard.request_id().to_string(),
            section: guard.section().to_string(),
            model: guard.model().to_string(),
            response_headers: response_headers.clone(),
            status: status.as_u16(),
            dumped: false,
        };
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
    let validated = crate::validate_upstream_body(body.clone(), guard.request_id())?;
    guard.response_dump(
        validated.dump,
        status.as_u16(),
        false,
        response_headers.clone(),
    );
    let mut response = Response::builder().status(status);
    for (name, value) in response_headers {
        response = response.header(name, value);
    }
    response
        .body(Body::from(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

fn copy_response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            if matches!(
                name,
                "content-type"
                    | "request-id"
                    | "x-request-id"
                    | "x-claude-code-session-id"
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
        .collect();
    // Map x-claude-code-session-id → x-request-id for OpenAI clients
    if !result.iter().any(|(n, _)| n == "x-request-id") {
        if let Some((_, v)) = result.iter().find(|(n, _)| n == "x-claude-code-session-id") {
            result.push(("x-request-id".into(), v.clone()));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── response header mapping tests ─────────────────────────

    /// Anthropic upstream sent x-claude-code-session-id → OpenAI client gets x-request-id.
    #[test]
    fn copy_response_headers_maps_x_claude_code_session_id_to_x_request_id() {
        let mut headers = HeaderMap::new();
        headers.insert("x-claude-code-session-id", "sess-1".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        let result = copy_response_headers(&headers);
        // x-claude-code-session-id relayed as-is for Anthropic clients
        assert!(result
            .iter()
            .any(|(n, v)| n == "x-claude-code-session-id" && v == "sess-1"));
        // Also mapped to x-request-id for OpenAI clients
        let found = result
            .iter()
            .any(|(n, v)| n == "x-request-id" && v == "sess-1");
        assert!(
            found,
            "x-claude-code-session-id must be mapped to x-request-id"
        );
    }
}
