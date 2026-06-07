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
use crate::error::AppError;
use crate::sse;

#[derive(Clone)]
pub struct AnthropicHandler {
    client: Client,
    dump_on_error: bool,
    hint_statuses: Arc<HashSet<StatusCode>>,
}

impl AnthropicHandler {
    pub fn new(
        config: &crate::config::Config,
        hint_statuses: Arc<HashSet<StatusCode>>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            client: Client::builder()
                .timeout(config.upstream_timeout)
                .build()
                .map_err(|err| AppError::Internal(err.to_string()))?,
            dump_on_error: config.dump_on_error,
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
        let model = crate::peek_model_from_json(&body);
        let messages_detail = crate::messages_detail_from_bytes(&body);
        let body = Bytes::from(crate::apply_token_caps(&body, route)?);
        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let upstream = builder.body(body).send().await?;
        if self.dump_on_error
            && !upstream.status().is_success()
            && !sse::is_event_stream(upstream.headers())
        {
            let status = upstream.status();
            let response_headers = copy_response_headers(upstream.headers());
            let error_body = upstream.text().await.unwrap_or_default();
            crate::dump_upstream_error(&crate::UpstreamErrorCtx {
                status: status.as_u16(),
                error_message: error_body.clone(),
                model,
                request_size,
                input_messages: None,
                max_tokens: None,
                chunks_received: None,
                bytes_received: None,
                messages_detail,
            });
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
        relay_upstream_response(upstream).await
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
                crate::dump_request_error(400, &format!("invalid OpenAI body: {err}"), body);
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

        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let upstream = builder.json(&anthropic_req).send().await?;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
            if self.dump_on_error {
                crate::dump_upstream_error(&crate::UpstreamErrorCtx {
                    status: status.as_u16(),
                    error_message: error_body.clone(),
                    model: openai_req.model.clone(),
                    request_size,
                    input_messages: Some(openai_req.messages.len()),
                    max_tokens: openai_req.max_tokens.or(openai_req.max_completion_tokens),
                    chunks_received: None,
                    bytes_received: None,
                    messages_detail: Some(crate::openai_messages_detail(&openai_req)),
                });
            }
            return relay_error_body(status, error_body, &self.hint_statuses);
        }

        let anthropic_resp: MessageResponse = upstream.json().await?;
        let openai_resp =
            translate_anthropic_to_openai_response(&anthropic_resp, &openai_req.model);
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

        let builder = self.build_upstream_request(request_headers, route, anthropic_endpoint)?;
        let upstream = builder.json(&anthropic_req).send().await?;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
            if self.dump_on_error {
                crate::dump_upstream_error(&crate::UpstreamErrorCtx {
                    status: status.as_u16(),
                    error_message: error_body.clone(),
                    model: openai_req.model.clone(),
                    request_size,
                    input_messages: Some(openai_req.messages.len()),
                    max_tokens: openai_req.max_tokens.or(openai_req.max_completion_tokens),
                    chunks_received: None,
                    bytes_received: None,
                    messages_detail: Some(crate::openai_messages_detail(openai_req)),
                });
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
