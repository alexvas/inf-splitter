use anyllm_translate::anthropic::StreamEvent;
use anyllm_translate::mapping::streaming_map::StreamingTranslator;
use anyllm_translate::openai::ChatCompletionChunk;
use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;

use crate::error::AppError;

/// Maximum length of a single SSE line before the connection is aborted.
/// Protects against unbounded buffer growth from a misbehaving upstream.
pub const MAX_SSE_LINE_LENGTH: usize = 1024 * 1024; // 1 MB

pub fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.contains("text/event-stream"))
        .unwrap_or(false)
}

pub fn parse_sse_line(
    line: &str,
    translator: &mut StreamingTranslator,
) -> Option<Vec<StreamEvent>> {
    let data = line.strip_prefix("data: ")?.trim();
    if data == "[DONE]" {
        return Some(translator.finish());
    }
    let chunk: ChatCompletionChunk = serde_json::from_str(data).ok()?;
    Some(translator.process_chunk(&chunk))
}

pub fn parse_anthropic_sse_event(line: &str) -> Option<StreamEvent> {
    let data = line.strip_prefix("data: ")?.trim();
    if data.is_empty() {
        return None;
    }
    serde_json::from_str(data).ok()
}

pub fn format_openai_sse_chunk(chunk: &ChatCompletionChunk) -> String {
    let payload = serde_json::to_string(chunk).unwrap_or_else(|_| "{}".to_string());
    format!("data: {payload}\n\n")
}

pub fn format_sse_event_str(event: &StreamEvent) -> String {
    let payload = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    format!("event: message\ndata: {payload}\n\n")
}

pub fn format_sse_event(event: &StreamEvent) -> bytes::Bytes {
    bytes::Bytes::from(format_sse_event_str(event))
}

pub fn sse_response<S>(request_headers: &HeaderMap, body: S) -> Result<Response, AppError>
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
