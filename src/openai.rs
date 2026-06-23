use std::collections::HashMap;
use std::sync::Arc;

use anyllm_translate::anthropic::MessageCreateRequest;
use anyllm_translate::mapping::streaming_map::StreamingTranslator;
use anyllm_translate::openai::{ChatCompletionRequest, ChatCompletionResponse};
use anyllm_translate::{translate_request, translate_response, TranslationConfig};
use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use reqwest::Client as HttpClient;

use crate::auth::{forward_request_headers, forward_request_headers_map};
use crate::config::{Config, ErrorTranslationRule, RouteTarget};
use crate::diagnostics::{Diagnostics, RequestDiagnostics, StatsEvent};
use crate::error::AppError;
use crate::relay::cap_openai_max_tokens;
use crate::relay::DiagnosticStream;
use crate::sse;

#[derive(Clone)]
pub struct OpenAiHandler {
    clients: HashMap<String, HttpClient>,
    diagnostics: Diagnostics,
    error_translation: Arc<[ErrorTranslationRule]>,
}

impl OpenAiHandler {
    pub fn new(config: &Config, diagnostics: Diagnostics) -> Result<Self, AppError> {
        Ok(Self {
            clients: crate::build_client_map(config)?,
            diagnostics,
            error_translation: config.error_translation.clone().into(),
        })
    }

    fn get_client(&self, proxy: Option<&str>) -> &HttpClient {
        proxy
            .and_then(|url| self.clients.get(url))
            .unwrap_or_else(|| self.clients.get("").expect("default client must exist"))
    }

    pub async fn handle_from_anthropic(
        &self,
        body: &[u8],
        request_headers: &HeaderMap,
        route: &RouteTarget,
        openai_endpoint: &str,
    ) -> Result<Response, AppError> {
        let body_len = body.len();
        let value = strip_adaptive_thinking(body);
        let mut req: MessageCreateRequest = serde_json::from_value(value).map_err(|err| {
            self.diagnostics.record_stats(&StatsEvent {
                section: route.section.clone(),
                request_id: self.diagnostics.new_request_id(),
                ts: crate::diagnostics::ts_string(),
                direction: "anthropic->openai".into(),
                model: "?".into(),
                upstream: openai_endpoint.into(),
                status: 400,
                duration_ms: 0,
                request_size_bytes: body_len,
                error: Some(format!("invalid Anthropic body: {err}")),
                ..Default::default()
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
        let mut value: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
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
        let body = serde_json::to_vec(&value).map_err(|e| AppError::Internal(e.to_string()))?;
        let backend_url = format!("{endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.get_client(route.proxy.as_deref())
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
        if let Some(ref body_bytes) = downstream_body {
            let egress_headers =
                forward_request_headers_map(route.api_key.as_deref(), request_headers);
            guard.egress_dump(body_bytes, &egress_headers);
        }
        let start = std::time::Instant::now();
        let upstream = builder.body(body).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;
        let is_err = !upstream.status().is_success() && !sse::is_event_stream(upstream.headers());
        if is_err {
            let status = upstream.status();
            let response_headers = upstream.headers().clone();
            let response_header_pairs: Vec<(String, String)> = response_headers
                .iter()
                .map(|(n, v)| {
                    (
                        n.as_str().to_string(),
                        v.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            let error_body = upstream.text().await.unwrap_or_default();
            guard.finish_with_upstream_error(
                status.as_u16(),
                duration_ms,
                request_size,
                endpoint,
                "openai->openai",
                false,
                error_body.clone(),
                response_header_pairs,
            );
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let error_body =
                crate::apply_error_translation(sc, error_body, &self.error_translation);
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
        let relayed = relay_openai_upstream(upstream, &guard).await?;
        let is_streaming = sse::is_event_stream(relayed.headers());
        guard.finish(
            relayed.status().as_u16(),
            duration_ms,
            request_size,
            None,
            endpoint,
            "openai->openai",
            is_streaming,
        );
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
        if route.max_tokens.is_some() && route.max_completion_tokens.is_none() {
            if let Some(limit) = route.max_tokens {
                if let Some(existing) = openai_req.max_completion_tokens {
                    if existing > limit {
                        openai_req.max_completion_tokens = Some(limit);
                    }
                }
            }
        }
        sanitize_openai_egress(&mut openai_req);
        openai_req.stream_options = None;

        let prepared =
            crate::prepare_egress_body(&openai_req, &req.model, route, &self.diagnostics)?;

        let guard = RequestDiagnostics::new(&self.diagnostics, &route.section, &req.model);
        if self.diagnostics.stats_enabled() {
            guard.set_input_messages(req.messages.len());
            guard.set_max_tokens(req.max_tokens);
            guard.set_messages_detail_ingress(crate::diagnostics::anthropic_messages_detail(req));
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
            serde_json::to_string(req).ok()
        } else {
            None
        };
        if let Some(ref s) = ingress_str {
            guard.ingress_dump(s.as_bytes(), request_headers);
        }

        let request_size = serde_json::to_vec(req).map(|v| v.len()).unwrap_or(0);
        let backend_url = format!("{openai_endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.get_client(route.proxy.as_deref())
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let start = std::time::Instant::now();
        let upstream = builder.body(prepared.bytes).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let response_headers = upstream.headers().clone();
            let response_header_pairs: Vec<(String, String)> = response_headers
                .iter()
                .map(|(n, v)| {
                    (
                        n.as_str().to_string(),
                        v.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            let error_body = upstream
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
            guard.finish_with_upstream_error(
                status.as_u16(),
                duration_ms,
                request_size,
                openai_endpoint,
                "anthropic->openai",
                false,
                error_body.clone(),
                response_header_pairs,
            );
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = crate::apply_error_translation(sc, error_body, &self.error_translation);
            return Response::builder()
                .status(sc)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .map_err(|err| AppError::Internal(err.to_string()));
        }

        let openai_resp: ChatCompletionResponse = upstream.json().await?;
        let response = translate_response(&openai_resp, &req.model);

        guard.finish(
            200,
            duration_ms,
            request_size,
            None,
            openai_endpoint,
            "anthropic->openai",
            false,
        );
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
        if route.max_tokens.is_some() && route.max_completion_tokens.is_none() {
            if let Some(limit) = route.max_tokens {
                if let Some(existing) = openai_req.max_completion_tokens {
                    if existing > limit {
                        openai_req.max_completion_tokens = Some(limit);
                    }
                }
            }
        }
        sanitize_openai_egress(&mut openai_req);
        openai_req.stream = Some(true);
        openai_req.stream_options = None;

        let prepared =
            crate::prepare_egress_body(&openai_req, &req.model, route, &self.diagnostics)?;

        let guard = RequestDiagnostics::new(&self.diagnostics, &route.section, &req.model);
        if self.diagnostics.stats_enabled() {
            guard.set_input_messages(req.messages.len());
            guard.set_max_tokens(req.max_tokens);
            guard.set_messages_detail_ingress(crate::diagnostics::anthropic_messages_detail(req));
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
            serde_json::to_string(req).ok()
        } else {
            None
        };
        if let Some(ref s) = ingress_str {
            guard.ingress_dump(s.as_bytes(), request_headers);
        }

        let request_size = serde_json::to_vec(req).map(|v| v.len()).unwrap_or(0);
        let backend_url = format!("{openai_endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.get_client(route.proxy.as_deref())
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let start = std::time::Instant::now();
        let upstream = builder.body(prepared.bytes).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let response_headers = upstream.headers().clone();
            let response_header_pairs: Vec<(String, String)> = response_headers
                .iter()
                .map(|(n, v)| {
                    (
                        n.as_str().to_string(),
                        v.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            let error_body = upstream
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
            guard.finish_with_upstream_error(
                status.as_u16(),
                duration_ms,
                request_size,
                openai_endpoint,
                "anthropic->openai",
                true,
                error_body.clone(),
                response_header_pairs,
            );
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = crate::apply_error_translation(sc, error_body, &self.error_translation);
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

        let resp = sse::sse_response(request_headers, sse_stream)?;

        guard.finish(
            200,
            duration_ms,
            request_size,
            None,
            openai_endpoint,
            "anthropic->openai",
            true,
        );

        Ok(resp)
    }
}

/// Forward relevant non-hop-by-hop headers from OpenAI upstream responses.
fn relay_response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            let lower = name.to_ascii_lowercase();
            if lower == "content-type"
                || lower.starts_with("x-ratelimit-")
                || lower == "x-request-id"
                || lower == "request-id"
                || lower == "x-claude-code-session-id"
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
        .collect();
    // Map x-request-id → x-claude-code-session-id for Anthropic (Claude CLI) clients
    if !result.iter().any(|(n, _)| n == "x-claude-code-session-id") {
        if let Some((_, v)) = result
            .iter()
            .find(|(n, _)| n == "x-request-id" || n == "request-id")
        {
            result.push(("x-claude-code-session-id".into(), v.clone()));
        }
    }
    result
}

async fn relay_openai_upstream(
    upstream: reqwest::Response,
    guard: &RequestDiagnostics,
) -> Result<Response, AppError> {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let relay_headers = relay_response_headers(&headers);

    if sse::is_event_stream(&headers) {
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

    let body = upstream.bytes().await?;
    let validated = crate::validate_upstream_body(body.clone(), guard.request_id())?;
    guard.response_dump(
        validated.dump,
        status.as_u16(),
        false,
        relay_headers.clone(),
    );
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
fn strip_adaptive_thinking(body: &[u8]) -> serde_json::Value {
    let mut value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return serde_json::Value::Null,
    };
    if let Some(thinking) = value.get("thinking") {
        if thinking.get("type").and_then(|t| t.as_str()) == Some("adaptive") {
            value.as_object_mut().and_then(|obj| obj.remove("thinking"));
        }
    }
    value
}

fn cap_anthropic_max_tokens(req: &mut MessageCreateRequest, limit: Option<u32>) {
    let Some(limit) = limit else {
        return;
    };
    if req.max_tokens > limit {
        req.max_tokens = limit;
    }
}

/// Strip Anthropic-specific fields that leak into OpenAI requests through
/// `anyllm_translate`'s `req.extra.clone()`, and replace `max_tokens` with
/// `max_completion_tokens` for newer OpenAI models that reject the legacy field.
fn sanitize_openai_egress(req: &mut ChatCompletionRequest) {
    req.extra.remove("context_management");
    req.extra.remove("output_config");
    if req.max_completion_tokens.is_some() {
        req.max_tokens = None;
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
        let v = strip_adaptive_thinking(body);
        assert!(v.get("thinking").is_none());
    }

    #[test]
    fn strip_adaptive_thinking_passes_through_other() {
        let body = br#"{"model":"x","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#;
        let original: serde_json::Value = serde_json::from_slice(body).unwrap();
        let cleaned = strip_adaptive_thinking(body);
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

    // ── sanitize_openai_egress ──────────────────────────────

    fn make_chat_completion_req() -> ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": "gpt-5.5",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 32000,
            "max_completion_tokens": 32000
        }))
        .unwrap()
    }

    #[test]
    fn sanitize_openai_egress_removes_context_management_from_extra() {
        let mut req = make_chat_completion_req();
        req.extra.insert(
            "context_management".into(),
            serde_json::json!({"edits": [{"keep": "all", "type": "clear_thinking_20251015"}]}),
        );
        sanitize_openai_egress(&mut req);
        assert!(
            req.extra.get("context_management").is_none(),
            "context_management must be removed"
        );
    }

    #[test]
    fn sanitize_openai_egress_removes_output_config_from_extra() {
        let mut req = make_chat_completion_req();
        req.extra
            .insert("output_config".into(), serde_json::json!({"effort": "max"}));
        sanitize_openai_egress(&mut req);
        assert!(
            req.extra.get("output_config").is_none(),
            "output_config must be removed"
        );
    }

    #[test]
    fn sanitize_openai_egress_does_not_remove_unrelated_extra_fields() {
        let mut req = make_chat_completion_req();
        req.extra.insert("seed".into(), serde_json::json!(42));
        sanitize_openai_egress(&mut req);
        assert_eq!(req.extra.get("seed").and_then(|v| v.as_u64()), Some(42));
    }

    #[test]
    fn sanitize_openai_egress_nulls_max_tokens_when_completion_tokens_present() {
        let mut req = make_chat_completion_req();
        req.max_tokens = Some(32000);
        req.max_completion_tokens = Some(32000);
        sanitize_openai_egress(&mut req);
        assert_eq!(req.max_tokens, None);
        assert_eq!(req.max_completion_tokens, Some(32000));
    }

    #[test]
    fn sanitize_openai_egress_preserves_max_tokens_when_completion_tokens_absent() {
        let mut req = make_chat_completion_req();
        req.max_tokens = Some(1024);
        req.max_completion_tokens = None;
        sanitize_openai_egress(&mut req);
        assert_eq!(req.max_tokens, Some(1024));
        assert_eq!(req.max_completion_tokens, None);
    }

    #[test]
    fn sanitize_openai_egress_is_idempotent() {
        let mut req = make_chat_completion_req();
        req.extra.insert(
            "context_management".into(),
            serde_json::json!({"edits": []}),
        );
        req.extra
            .insert("output_config".into(), serde_json::json!({"effort": "max"}));
        req.max_tokens = Some(32000);
        req.max_completion_tokens = Some(32000);

        sanitize_openai_egress(&mut req);
        sanitize_openai_egress(&mut req); // second call must not panic

        assert!(req.extra.get("context_management").is_none());
        assert!(req.extra.get("output_config").is_none());
        assert_eq!(req.max_tokens, None);
        assert_eq!(req.max_completion_tokens, Some(32000));
    }

    // ── relay_response_headers ────────────────────────────────

    /// OpenAI upstream sent x-request-id → Anthropic client gets x-claude-code-session-id.
    #[test]
    fn relay_response_headers_maps_x_request_id_to_x_claude_code_session_id() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "req-abc".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        let result = relay_response_headers(&headers);
        let has_mapped = result
            .iter()
            .any(|(n, v)| n == "x-claude-code-session-id" && v == "req-abc");
        assert!(
            has_mapped,
            "x-request-id must be mapped to x-claude-code-session-id"
        );
    }
}
