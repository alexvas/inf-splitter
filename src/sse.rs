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
    let event_name = match event {
        StreamEvent::MessageStart { .. } => "message_start",
        StreamEvent::ContentBlockStart { .. } => "content_block_start",
        StreamEvent::ContentBlockDelta { .. } => "content_block_delta",
        StreamEvent::ContentBlockStop { .. } => "content_block_stop",
        StreamEvent::MessageDelta { .. } => "message_delta",
        StreamEvent::MessageStop { .. } => "message_stop",
        StreamEvent::Ping { .. } => "ping",
        StreamEvent::Error { .. } => "error",
        // Catch-all for forward-compat (e.g. Unknown added in anyllm_translate 0.9.7+).
        _ => "message",
    };
    format!("event: {event_name}\ndata: {payload}\n\n")
}

pub fn format_sse_event(event: &StreamEvent) -> bytes::Bytes {
    bytes::Bytes::from(format_sse_event_str(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_event_stream_detects_valid() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
        assert!(is_event_stream(&headers));
    }

    #[test]
    fn is_event_stream_rejects_json() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        assert!(!is_event_stream(&headers));
    }

    #[test]
    fn is_event_stream_empty_headers() {
        assert!(!is_event_stream(&HeaderMap::new()));
    }

    #[test]
    fn parse_anthropic_sse_event_parses_message_start() {
        let line = "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}";
        let event = parse_anthropic_sse_event(line).expect("should parse");
        assert!(matches!(event, StreamEvent::MessageStart { .. }));
    }

    #[test]
    fn parse_anthropic_sse_event_ignores_empty_data() {
        assert!(parse_anthropic_sse_event("data: ").is_none());
        assert!(parse_anthropic_sse_event("data:").is_none());
    }

    #[test]
    fn parse_anthropic_sse_event_returns_none_on_garbage() {
        assert!(parse_anthropic_sse_event("data: not json").is_none());
        assert!(parse_anthropic_sse_event("no prefix").is_none());
    }

    #[test]
    fn format_openai_sse_chunk_produces_valid_sse() {
        let chunk: ChatCompletionChunk = serde_json::from_value(serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "gpt-4",
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
        }))
        .unwrap();
        let result = format_openai_sse_chunk(&chunk);
        assert!(result.starts_with("data: "));
        assert!(result.ends_with("\n\n"));
    }

    #[test]
    fn format_sse_event_str_uses_correct_event_names() {
        // Construct variants via JSON deserialization (internally tagged on "type").
        let message_start: StreamEvent = serde_json::from_value(serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_1", "type": "message", "role": "assistant",
                "model": "claude", "content": [], "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }
        }))
        .unwrap();

        let content_block_start: StreamEvent = serde_json::from_value(serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }))
        .unwrap();

        let content_block_delta: StreamEvent = serde_json::from_value(serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "hi"}
        }))
        .unwrap();

        let content_block_stop = StreamEvent::ContentBlockStop { index: 0 };
        let message_stop = StreamEvent::MessageStop {};
        let ping = StreamEvent::Ping {};

        let message_delta: StreamEvent = serde_json::from_value(serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 5}
        }))
        .unwrap();

        let error_event: StreamEvent = serde_json::from_value(serde_json::json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "busy"}
        }))
        .unwrap();

        let cases: &[(&StreamEvent, &str)] = &[
            (&message_start, "message_start"),
            (&content_block_start, "content_block_start"),
            (&content_block_delta, "content_block_delta"),
            (&content_block_stop, "content_block_stop"),
            (&message_delta, "message_delta"),
            (&message_stop, "message_stop"),
            (&ping, "ping"),
            (&error_event, "error"),
        ];

        for (event, expected_name) in cases {
            let result = format_sse_event_str(event);
            assert!(
                result.starts_with(&format!("event: {expected_name}\ndata: ")),
                "expected event: {expected_name}, got: {result:?}"
            );
            assert!(result.ends_with("\n\n"));
        }
    }

    #[test]
    fn sse_response_sets_correct_headers() {
        let headers = HeaderMap::new();
        let stream = futures::stream::empty::<Result<bytes::Bytes, std::io::Error>>();
        let response = sse_response(&headers, stream).expect("should build SSE response");
        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        assert_eq!(ct, Some("text/event-stream"));
    }

    #[test]
    fn sse_response_does_not_echo_accept_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, "application/json".parse().unwrap());
        let stream = futures::stream::empty::<Result<bytes::Bytes, std::io::Error>>();
        let response = sse_response(&headers, stream).expect("should build SSE response");
        assert!(
            response.headers().get(header::ACCEPT).is_none(),
            "Accept header must not be echoed on SSE response"
        );
    }
}

pub fn sse_response<S>(_request_headers: &HeaderMap, body: S) -> Result<Response, AppError>
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}

/// Like [`sse_response`] but adds an extra header to the response.
pub fn sse_response_with_extra_header<S>(
    _request_headers: &HeaderMap,
    body: S,
    extra_name: &str,
    extra_value: &str,
) -> Result<Response, AppError>
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header(extra_name, extra_value)
        .body(Body::from_stream(body))
        .map_err(|err| AppError::Internal(err.to_string()))
}
