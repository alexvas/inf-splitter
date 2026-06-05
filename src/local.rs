use std::collections::HashSet;
use std::sync::Arc;

use anyllm_client::{Auth, Client, ClientConfig, ClientError, HttpClientConfig};
use anyllm_translate::anthropic::MessageCreateRequest;
use anyllm_translate::mapping::streaming_map::StreamingTranslator;
use anyllm_translate::{translate_request, translate_response, TranslationConfig};
use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use reqwest::Client as HttpClient;

use crate::auth::forward_request_headers;
use crate::config::{cap_numeric_field, Config, RouteTarget};
use crate::error::AppError;
use crate::sse;

#[derive(Clone)]
pub struct OpenAiHandler {
    http: HttpClient,
    omit_stream_options: bool,
    hint_statuses: Arc<HashSet<StatusCode>>,
}

impl OpenAiHandler {
    pub fn new(config: &Config, hint_statuses: Arc<HashSet<StatusCode>>) -> Result<Self, AppError> {
        Ok(Self {
            http: HttpClient::builder()
                .timeout(config.upstream_timeout)
                .build()
                .map_err(|err| AppError::Internal(err.to_string()))?,
            omit_stream_options: config.omit_stream_options,
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
        let client = self.build_client(route, openai_endpoint, &req.model, request_headers)?;

        if req.stream.unwrap_or(false) {
            self.handle_stream(&req, request_headers, route, openai_endpoint, &client)
                .await
        } else if self.omit_stream_options {
            self.handle_sync_manual(&req, request_headers, route, openai_endpoint)
                .await
        } else {
            self.handle_sync_client(&req, &client).await
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
        let body = apply_token_caps(body, route)?;
        let backend_url = format!("{endpoint}/v1/chat/completions");
        let builder = forward_request_headers(
            self.http
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let upstream = builder.body(body).send().await?;
        relay_openai_upstream(upstream).await
    }

    fn build_client(
        &self,
        route: &RouteTarget,
        openai_endpoint: &str,
        model: &str,
        request_headers: &HeaderMap,
    ) -> Result<Client, AppError> {
        let backend_url = format!("{openai_endpoint}/v1/chat/completions");
        let mut translation = TranslationConfig::builder();
        for mapped in route.model_names.iter().chain([model.to_string()].iter()) {
            translation = translation.model_map(mapped, mapped);
        }

        let http = HttpClientConfig {
            ssrf_protection: false,
            ..Default::default()
        };

        let client_config = ClientConfig::builder()
            .backend_url(backend_url)
            .auth(Self::resolve_bearer_auth(route, request_headers))
            .http(http)
            .translation(translation.build())
            .build();

        Ok(Client::new(client_config))
    }

    fn resolve_bearer_auth(route: &RouteTarget, request_headers: &HeaderMap) -> Auth {
        if let Some(key) = &route.api_key {
            return Auth::Bearer(key.clone());
        }
        if let Some(key) = request_headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
        {
            return Auth::Bearer(key.to_string());
        }
        if let Some(value) = request_headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
        {
            if let Some(token) = value.strip_prefix("Bearer ") {
                return Auth::Bearer(token.to_string());
            }
        }
        Auth::Bearer("ollama".into())
    }

    fn translation_for(&self, route: &RouteTarget, model: &str) -> TranslationConfig {
        let mut builder = TranslationConfig::builder();
        for mapped in route.model_names.iter().chain([model.to_string()].iter()) {
            builder = builder.model_map(mapped, mapped);
        }
        builder.build()
    }

    async fn handle_sync_client(
        &self,
        req: &MessageCreateRequest,
        client: &Client,
    ) -> Result<Response, AppError> {
        let response = client.messages(req).await?;
        Ok((StatusCode::OK, axum::Json(response)).into_response())
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

        let client = self.build_client(route, openai_endpoint, &req.model, request_headers)?;
        let (openai_resp, _, _) = client.chat_completion(&openai_req).await?;
        let response = translate_response(&openai_resp, &req.model);
        Ok((StatusCode::OK, axum::Json(response)).into_response())
    }

    async fn handle_stream(
        &self,
        req: &MessageCreateRequest,
        request_headers: &HeaderMap,
        route: &RouteTarget,
        openai_endpoint: &str,
        client: &Client,
    ) -> Result<Response, AppError> {
        if self.omit_stream_options {
            self.handle_stream_manual(req, request_headers, route, openai_endpoint)
                .await
        } else {
            self.handle_stream_client(req, request_headers, client)
                .await
        }
    }

    async fn handle_stream_client(
        &self,
        req: &MessageCreateRequest,
        request_headers: &HeaderMap,
        client: &Client,
    ) -> Result<Response, AppError> {
        let (stream, _rate_limits) = client.messages_stream(req).await?;
        sse::sse_response(
            request_headers,
            stream.map(|event| {
                event
                    .map(|stream_event| sse::format_sse_event(&stream_event))
                    .map_err(|err| std::io::Error::other(err.to_string()))
            }),
        )
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
            return relay_upstream_status_error(upstream, &self.hint_statuses).await;
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
                            buffer.push_str(&String::from_utf8_lossy(&chunk));
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

impl From<anyllm_client::ClientError> for AppError {
    fn from(err: ClientError) -> Self {
        Self::Upstream(err.to_string())
    }
}

async fn relay_upstream_status_error(
    upstream: reqwest::Response,
    hint_statuses: &HashSet<StatusCode>,
) -> Result<Response, AppError> {
    let status = upstream.status();
    let body = upstream
        .text()
        .await
        .unwrap_or_else(|e| format!("(failed to read error body: {e})"));
    let status_code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = append_size_hint(status_code, body, hint_statuses);
    Response::builder()
        .status(status_code)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

fn append_size_hint(
    status: StatusCode,
    body: String,
    hint_statuses: &HashSet<StatusCode>,
) -> String {
    if !hint_statuses.contains(&status) {
        return body;
    }
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(serde_json::Value::String(msg)) = value.pointer_mut("/error/message") {
            *msg = format!("{msg}. Try reducing context size or splitting into smaller requests.");
            return serde_json::to_string(&value).unwrap_or(body);
        }
    }
    format!("{body}. Try reducing context size or splitting into smaller requests.")
}

fn apply_token_caps(body: &[u8], route: &RouteTarget) -> Result<Vec<u8>, AppError> {
    let has_caps = route.max_tokens.is_some()
        || route.max_output_tokens.is_some()
        || route.max_completion_tokens.is_some();
    if !has_caps {
        return Ok(body.to_vec());
    }
    let mut value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
    if let Some(limit) = route.max_tokens {
        cap_numeric_field(&mut value, "max_tokens", limit);
    }
    if let Some(limit) = route.max_output_tokens {
        cap_numeric_field(&mut value, "max_output_tokens", limit);
    }
    if let Some(limit) = route.max_completion_tokens {
        cap_numeric_field(&mut value, "max_completion_tokens", limit);
    }
    serde_json::to_vec(&value).map_err(|e| AppError::Internal(e.to_string()))
}

fn cap_anthropic_max_tokens(req: &mut MessageCreateRequest, limit: Option<u32>) {
    let Some(limit) = limit else {
        return;
    };
    if req.max_tokens > limit {
        req.max_tokens = limit;
    }
}
