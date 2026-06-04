use anyllm_client::{Auth, Client, ClientConfig, ClientError, HttpClientConfig};
use anyllm_translate::anthropic::{MessageCreateRequest, StreamEvent};
use anyllm_translate::mapping::streaming_map::StreamingTranslator;
use anyllm_translate::openai::ChatCompletionChunk;
use anyllm_translate::{translate_request, translate_response, TranslationConfig};
use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use reqwest::Client as HttpClient;

use crate::auth::apply_upstream_auth;
use crate::config::{Config, RouteTarget};
use crate::error::AppError;

#[derive(Clone)]
pub struct OpenAiHandler {
    http: HttpClient,
    omit_stream_options: bool,
}

impl OpenAiHandler {
    pub fn new(config: &Config) -> Result<Self, AppError> {
        Ok(Self {
            http: HttpClient::builder()
                .build()
                .map_err(|err| AppError::Internal(err.to_string()))?,
            omit_stream_options: config.omit_stream_options,
        })
    }

    pub async fn handle_from_anthropic(
        &self,
        body: &[u8],
        request_headers: &HeaderMap,
        route: &RouteTarget,
    ) -> Result<Response, AppError> {
        let req: MessageCreateRequest = serde_json::from_slice(body)?;
        let client = self.build_client(route, &req.model, request_headers)?;

        if req.stream.unwrap_or(false) {
            self.handle_stream(&req, request_headers, route, &client)
                .await
        } else if self.omit_stream_options {
            self.handle_sync_manual(&req, request_headers, route).await
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
    ) -> Result<Response, AppError> {
        let backend_url = format!("{}/v1/chat/completions", route.endpoint);
        let builder = apply_upstream_auth(
            self.http
                .post(&backend_url)
                .header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let upstream = builder.body(body.to_vec()).send().await?;
        relay_openai_upstream(upstream).await
    }

    fn build_client(
        &self,
        route: &RouteTarget,
        model: &str,
        request_headers: &HeaderMap,
    ) -> Result<Client, AppError> {
        let backend_url = format!("{}/v1/chat/completions", route.endpoint);
        let mut translation = TranslationConfig::builder();
        for mapped in route.model_names.iter().chain([model.to_string()].iter()) {
            translation = translation.model_map(mapped, mapped);
        }

        let mut http = HttpClientConfig::default();
        http.ssrf_protection = false;

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

    async fn handle_sync_client(&self, req: &MessageCreateRequest, client: &Client) -> Result<Response, AppError> {
        let response = client.messages(req).await?;
        Ok((StatusCode::OK, axum::Json(response)).into_response())
    }

    async fn handle_sync_manual(
        &self,
        req: &MessageCreateRequest,
        request_headers: &HeaderMap,
        route: &RouteTarget,
    ) -> Result<Response, AppError> {
        let translation = self.translation_for(route, &req.model);
        let mut openai_req = translate_request(req, &translation)
            .map_err(|err| AppError::Upstream(err.to_string()))?;
        openai_req.stream_options = None;

        let client = self.build_client(route, &req.model, request_headers)?;
        let (openai_resp, _, _) = client.chat_completion(&openai_req).await?;
        let response = translate_response(&openai_resp, &req.model);
        Ok((StatusCode::OK, axum::Json(response)).into_response())
    }

    async fn handle_stream(
        &self,
        req: &MessageCreateRequest,
        request_headers: &HeaderMap,
        route: &RouteTarget,
        client: &Client,
    ) -> Result<Response, AppError> {
        if self.omit_stream_options {
            self.handle_stream_manual(req, request_headers, route).await
        } else {
            self.handle_stream_client(req, request_headers, client).await
        }
    }

    async fn handle_stream_client(
        &self,
        req: &MessageCreateRequest,
        request_headers: &HeaderMap,
        client: &Client,
    ) -> Result<Response, AppError> {
        let (stream, _rate_limits) = client.messages_stream(req).await?;
        Ok(sse_response(
            request_headers,
            stream.map(|event| {
                event
                    .map(|stream_event| format_sse_event(&stream_event))
                    .map_err(|err| std::io::Error::other(err.to_string()))
            }),
        )?)
    }

    async fn handle_stream_manual(
        &self,
        req: &MessageCreateRequest,
        request_headers: &HeaderMap,
        route: &RouteTarget,
    ) -> Result<Response, AppError> {
        let translation = self.translation_for(route, &req.model);
        let mut openai_req = translate_request(req, &translation)
            .map_err(|err| AppError::Upstream(err.to_string()))?;
        openai_req.stream = Some(true);
        openai_req.stream_options = None;

        let backend_url = format!("{}/v1/chat/completions", route.endpoint);
        let builder = apply_upstream_auth(
            self.http.post(&backend_url).header(header::CONTENT_TYPE, "application/json"),
            request_headers,
            route.api_key.as_deref(),
        );

        let upstream = builder.json(&openai_req).send().await?;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let body = upstream.text().await.unwrap_or_default();
            return Err(AppError::Upstream(format!("HTTP {status}: {body}")));
        }

        let model = req.model.clone();
        let byte_stream = upstream.bytes_stream().map(|chunk| {
            chunk.map_err(|err| std::io::Error::other(err.to_string()))
        });

        let sse_stream = futures::stream::unfold(
            (byte_stream, StreamingTranslator::new(model), String::new()),
            |(mut byte_stream, mut translator, mut buffer)| async move {
                loop {
                    if let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim_end_matches('\r').to_string();
                        buffer = buffer[line_end + 1..].to_string();
                        if let Some(events) = parse_sse_line(&line, &mut translator) {
                            let payload = events
                                .iter()
                                .map(format_sse_event_str)
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
                                .map(format_sse_event_str)
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

        sse_response(request_headers, sse_stream)
    }
}

async fn relay_openai_upstream(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = upstream.status();
    let headers = upstream.headers().clone();

    if is_openai_event_stream(&headers) {
        let stream = upstream.bytes_stream().map(|chunk| {
            chunk.map_err(|err| std::io::Error::other(err.to_string()))
        });
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

fn is_openai_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.contains("text/event-stream"))
        .unwrap_or(false)
}

fn parse_sse_line(line: &str, translator: &mut StreamingTranslator) -> Option<Vec<StreamEvent>> {
    let data = line.strip_prefix("data: ")?.trim();
    if data == "[DONE]" {
        return Some(translator.finish());
    }
    let chunk: ChatCompletionChunk = serde_json::from_str(data).ok()?;
    Some(translator.process_chunk(&chunk))
}

fn sse_response<S>(request_headers: &HeaderMap, body: S) -> Result<Response, AppError>
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    let accept = request_headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("text/event-stream");

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header(header::ACCEPT, accept)
        .body(Body::from_stream(body))
        .map_err(|err| AppError::Internal(err.to_string()))?)
}

fn format_sse_event_str(event: &StreamEvent) -> String {
    let payload = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    format!("event: message\ndata: {payload}\n\n")
}

fn format_sse_event(event: &StreamEvent) -> bytes::Bytes {
    bytes::Bytes::from(format_sse_event_str(event))
}

impl From<anyllm_client::ClientError> for AppError {
    fn from(err: ClientError) -> Self {
        Self::Upstream(err.to_string())
    }
}
