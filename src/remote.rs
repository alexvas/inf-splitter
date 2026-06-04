use anyllm_translate::anthropic::{MessageResponse, StreamEvent};
use anyllm_translate::mapping::reverse_streaming_map::ReverseStreamingTranslator;
use anyllm_translate::openai::{ChatCompletionChunk, ChatCompletionRequest};
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

use crate::auth::{apply_upstream_auth, should_forward_request_header};
use crate::config::RouteTarget;
use crate::error::AppError;

#[derive(Clone)]
pub struct AnthropicHandler {
    client: Client,
}

impl AnthropicHandler {
    pub fn new(config: &crate::config::Config) -> Self {
        Self {
            client: Client::builder()
                .timeout(config.upstream_timeout)
                .build()
                .expect("reqwest client"),
        }
    }

    /// Anthropic ingress → Anthropic upstream (passthrough).
    pub async fn handle_from_anthropic(
        &self,
        body: Bytes,
        request_headers: &HeaderMap,
        route: &RouteTarget,
    ) -> Result<Response, AppError> {
        let builder = self.build_upstream_request(request_headers, route)?;
        let upstream = builder.body(body).send().await?;
        relay_upstream_response(upstream).await
    }

    /// OpenAI ingress → Anthropic upstream (translate request/response).
    pub async fn handle_from_openai(
        &self,
        body: &[u8],
        request_headers: &HeaderMap,
        route: &RouteTarget,
    ) -> Result<Response, AppError> {
        let openai_req: ChatCompletionRequest = serde_json::from_slice(body)?;

        if openai_req.stream.unwrap_or(false) {
            return self
                .handle_from_openai_stream(&openai_req, request_headers, route)
                .await;
        }

        let mut warnings = TranslationWarnings::default();
        let anthropic_req = translate_openai_to_anthropic_request(&openai_req, &mut warnings)
            .map_err(|err| AppError::BadRequest(err.to_string()))?;

        let builder = self.build_upstream_request(request_headers, route)?;
        let upstream = builder.json(&anthropic_req).send().await?;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let body = upstream.text().await.unwrap_or_default();
            return relay_error_body(status, body);
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
    ) -> Result<Response, AppError> {
        let mut warnings = TranslationWarnings::default();
        let mut anthropic_req = translate_openai_to_anthropic_request(openai_req, &mut warnings)
            .map_err(|err| AppError::BadRequest(err.to_string()))?;
        anthropic_req.stream = Some(true);

        let builder = self.build_upstream_request(request_headers, route)?;
        let upstream = builder.json(&anthropic_req).send().await?;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let body = upstream.text().await.unwrap_or_default();
            return relay_error_body(status, body);
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
                        if let Some(event) = parse_anthropic_sse_event(&line) {
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
                                    let payload = format_openai_sse_chunk(&chunk);
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
                            buffer.push_str(&String::from_utf8_lossy(&chunk));
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

        openai_sse_response(request_headers, sse_stream)
    }

    fn build_upstream_request(
        &self,
        request_headers: &HeaderMap,
        route: &RouteTarget,
    ) -> Result<RequestBuilder, AppError> {
        let url = format!("{}/v1/messages", route.endpoint);
        let mut builder = self
            .client
            .post(url)
            .header(header::CONTENT_TYPE, "application/json");

        if route.api_key.is_some() {
            builder = apply_upstream_auth(builder, request_headers, route.api_key.as_deref());
        } else {
            for (name, value) in request_headers.iter() {
                if should_forward_request_header(name.as_str()) {
                    if let Ok(value) = value.to_str() {
                        builder = builder.header(name.as_str(), value);
                    }
                }
            }
        }

        Ok(builder)
    }
}

fn parse_anthropic_sse_event(line: &str) -> Option<StreamEvent> {
    let data = line.strip_prefix("data: ")?.trim();
    if data.is_empty() {
        return None;
    }
    serde_json::from_str(data).ok()
}

fn format_openai_sse_chunk(chunk: &ChatCompletionChunk) -> String {
    let payload = serde_json::to_string(chunk).unwrap_or_else(|_| "{}".to_string());
    format!("data: {payload}\n\n")
}

fn openai_sse_response<S>(request_headers: &HeaderMap, body: S) -> Result<Response, AppError>
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    let accept = request_headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("text/event-stream");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header(header::ACCEPT, accept)
        .body(Body::from_stream(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

fn relay_error_body(status: reqwest::StatusCode, body: String) -> Result<Response, AppError> {
    Response::builder()
        .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

async fn relay_upstream_response(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = upstream.status();
    let response_headers = copy_response_headers(upstream.headers());

    if is_event_stream(upstream.headers()) {
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

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.contains("text/event-stream"))
        .unwrap_or(false)
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
