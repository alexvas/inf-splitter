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
use crate::error::AppError;
use crate::sse;

#[derive(Clone)]
pub struct OpenAiHandler {
    http: HttpClient,
    dump_on_error: bool,
    hint_statuses: Arc<HashSet<StatusCode>>,
}

impl OpenAiHandler {
    pub fn new(config: &Config, hint_statuses: Arc<HashSet<StatusCode>>) -> Result<Self, AppError> {
        Ok(Self {
            http: HttpClient::builder()
                .timeout(config.upstream_timeout)
                .build()
                .map_err(|err| AppError::Internal(err.to_string()))?,
            dump_on_error: config.dump_on_error,
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
        let mut req: MessageCreateRequest = serde_json::from_slice(body)?;
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
        let model = crate::peek_model_from_json(body);
        let messages_detail = crate::messages_detail_from_bytes(body);
        let body = crate::apply_token_caps(body, route)?;
        let backend_url = format!("{endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.http
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let upstream = builder.body(body).send().await?;
        if self.dump_on_error
            && !upstream.status().is_success()
            && !sse::is_event_stream(upstream.headers())
        {
            let status = upstream.status();
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
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            return Response::builder()
                .status(sc)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(error_body))
                .map_err(|err| AppError::Internal(err.to_string()));
        }
        relay_openai_upstream(upstream).await
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
        openai_req.stream_options = None;

        let backend_url = format!("{openai_endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.http
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let upstream = builder.json(&openai_req).send().await?;

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
                    model: req.model.clone(),
                    request_size: serde_json::to_vec(req).map(|v| v.len()).unwrap_or(0),
                    input_messages: Some(req.messages.len()),
                    max_tokens: Some(req.max_tokens),
                    chunks_received: None,
                    bytes_received: None,
                    messages_detail: Some(crate::anthropic_messages_detail(req)),
                });
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
        openai_req.stream = Some(true);
        openai_req.stream_options = None;

        let backend_url = format!("{openai_endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.http
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let upstream = builder.json(&openai_req).send().await?;

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
                    model: req.model.clone(),
                    request_size: serde_json::to_vec(req).map(|v| v.len()).unwrap_or(0),
                    input_messages: Some(req.messages.len()),
                    max_tokens: Some(req.max_tokens),
                    chunks_received: None,
                    bytes_received: None,
                    messages_detail: Some(crate::anthropic_messages_detail(req)),
                });
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

        sse::sse_response(request_headers, sse_stream)
    }
}

async fn relay_openai_upstream(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = upstream.status();
    let headers = upstream.headers().clone();

    if sse::is_event_stream(&headers) {
        let stream = upstream
            .bytes_stream()
            .map(|chunk| chunk.map_err(|err| std::io::Error::other(err.to_string())));
        let mut response = Response::builder().status(status);
        if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
            if let Ok(value) = content_type.to_str() {
                response = response.header(header::CONTENT_TYPE, value);
            }
        } else {
            response = response.header(header::CONTENT_TYPE, "text/event-stream");
        }
        response = response
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive");
        return response
            .body(Body::from_stream(stream))
            .map_err(|err| AppError::Internal(err.to_string()));
    }

    let body = upstream.bytes().await?;
    let mut response = Response::builder().status(status);
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        if let Ok(value) = content_type.to_str() {
            response = response.header(header::CONTENT_TYPE, value);
        }
    }
    response
        .body(Body::from(body))
        .map_err(|err| AppError::Internal(err.to_string()))
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
