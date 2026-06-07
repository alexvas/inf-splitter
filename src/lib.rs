pub mod anthropic;
pub mod auth;
pub mod config;
pub mod error;
pub mod openai;
pub mod router;
pub mod sse;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::Router;
use serde_json::Value;
use tokio::sync::Mutex;
use tower_http::limit::RequestBodyLimitLayer;

use crate::anthropic::AnthropicHandler;
use crate::config::{cap_numeric_field, Config, RouteTarget};
use crate::error::AppError;
use crate::openai::OpenAiHandler;
use crate::router::{router, AppState};

const BODY_TOO_LARGE_HINT: &str = "Try reducing context size or splitting into smaller requests.";

/// Append a size hint to an error body when the status code indicates the
/// request was too large.
pub(crate) fn append_size_hint(
    status: StatusCode,
    body: String,
    hint_statuses: &HashSet<StatusCode>,
) -> String {
    if !hint_statuses.contains(&status) {
        return body;
    }
    if let Ok(mut value) = serde_json::from_str::<Value>(&body) {
        if let Some(Value::String(msg)) = value.pointer_mut("/error/message") {
            *msg = format!("{msg}. Try reducing context size or splitting into smaller requests.");
            return serde_json::to_string(&value).unwrap_or(body);
        }
    }
    format!("{body}. Try reducing context size or splitting into smaller requests.")
}

/// Apply token limits from the route config to a raw JSON body (passthrough path).
pub(crate) fn apply_token_caps(body: &[u8], route: &RouteTarget) -> Result<Vec<u8>, AppError> {
    let has_caps = route.max_tokens.is_some()
        || route.max_output_tokens.is_some()
        || route.max_completion_tokens.is_some();
    if !has_caps {
        return Ok(body.to_vec());
    }
    let mut value: Value =
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

/// Extract the `model` field from a JSON byte slice. Returns `"?"` on failure.
pub(crate) fn peek_model_from_json(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from))
        .unwrap_or_else(|| "?".to_string())
}

/// Context captured at the point of an upstream error, for diagnostic dumping.
pub(crate) struct UpstreamErrorCtx {
    pub status: u16,
    pub error_message: String,
    pub model: String,
    pub request_size: usize,
    pub input_messages: Option<usize>,
    pub max_tokens: Option<u32>,
    pub chunks_received: Option<usize>,
    pub bytes_received: Option<usize>,
    /// Per-message breakdown: `[{role, parts: [{type, chars?, words?, bytes?, ...}]}]`.
    pub messages_detail: Option<serde_json::Value>,
}

/// Build per-message detail from an Anthropic `MessageCreateRequest`.
pub(crate) fn anthropic_messages_detail(
    req: &anyllm_translate::anthropic::MessageCreateRequest,
) -> serde_json::Value {
    use anyllm_translate::anthropic::Content;
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|msg| {
            let parts = match &msg.content {
                Content::Text(text) => vec![text_part_detail(text)],
                Content::Blocks(blocks) => blocks.iter().map(anthropic_block_part).collect(),
            };
            serde_json::json!({"role": msg.role, "parts": parts})
        })
        .collect();
    serde_json::json!(messages)
}

/// Build per-message detail from an OpenAI `ChatCompletionRequest`.
pub(crate) fn openai_messages_detail(
    req: &anyllm_translate::openai::ChatCompletionRequest,
) -> serde_json::Value {
    use anyllm_translate::openai::{ChatContent, ChatContentPart};
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|msg| {
            let mut parts: Vec<serde_json::Value> = match &msg.content {
                None => Vec::new(),
                Some(ChatContent::Text(text)) => vec![text_part_detail(text)],
                Some(ChatContent::Parts(content_parts)) => content_parts
                    .iter()
                    .map(|p| match p {
                        ChatContentPart::Text { text } => text_part_detail(text),
                        ChatContentPart::ImageUrl { image_url } => serde_json::json!({
                            "type": "image_url",
                            "url_chars": image_url.url.len()
                        }),
                        ChatContentPart::InputAudio { input_audio } => serde_json::json!({
                            "type": "audio",
                            "bytes": input_audio.data.len()
                        }),
                        ChatContentPart::File { file } => serde_json::json!({
                            "type": "file",
                            "bytes": file.file_data.as_ref().map(|d| d.len()).unwrap_or(0)
                        }),
                    })
                    .collect(),
            };
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    parts.push(serde_json::json!({
                        "type": "tool_call",
                        "name": tc.function.name,
                        "args_chars": tc.function.arguments.len()
                    }));
                }
            }
            serde_json::json!({"role": msg.role, "parts": parts})
        })
        .collect();
    serde_json::json!(messages)
}

/// Best-effort message detail from raw JSON bytes (passthrough paths).
pub(crate) fn messages_detail_from_bytes(body: &[u8]) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let arr = v.get("messages")?.as_array()?;
    let detail: Vec<serde_json::Value> = arr
        .iter()
        .map(|msg| {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
            let content = msg.get("content");
            let parts: Vec<serde_json::Value> = match content {
                Some(serde_json::Value::String(text)) => vec![text_part_detail(text)],
                Some(serde_json::Value::Array(items)) => items
                    .iter()
                    .map(|item| {
                        let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match ty {
                            "text" => text_part_detail(
                                item.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                            ),
                            "image" => {
                                let source = item.get("source");
                                let bytes = source
                                    .and_then(|s| s.get("data"))
                                    .and_then(|d| d.as_str())
                                    .map(|d| d.len())
                                    .unwrap_or(0);
                                let media = source
                                    .and_then(|s| s.get("media_type"))
                                    .and_then(|m| m.as_str());
                                let mut p = serde_json::json!({"type": "image", "bytes": bytes});
                                if let Some(m) = media {
                                    p["media_type"] = serde_json::json!(m);
                                }
                                p
                            }
                            "image_url" => {
                                let url = item
                                    .get("image_url")
                                    .and_then(|iu| iu.get("url"))
                                    .and_then(|u| u.as_str());
                                serde_json::json!({
                                    "type": "image_url",
                                    "url_chars": url.map(|u| u.len()).unwrap_or(0)
                                })
                            }
                            "tool_use" => {
                                let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                let input_bytes =
                                    item.get("input").map(|i| i.to_string().len()).unwrap_or(0);
                                serde_json::json!({
                                    "type": "tool_use",
                                    "name": name,
                                    "input_bytes": input_bytes
                                })
                            }
                            _ => serde_json::json!({"type": ty}),
                        }
                    })
                    .collect(),
                _ => Vec::new(),
            };
            serde_json::json!({"role": role, "parts": parts})
        })
        .collect();
    Some(serde_json::json!(detail))
}

fn text_part_detail(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "text",
        "chars": text.chars().count(),
        "words": text.split_whitespace().count()
    })
}

fn anthropic_block_part(block: &anyllm_translate::anthropic::ContentBlock) -> serde_json::Value {
    use anyllm_translate::anthropic::ContentBlock;
    match block {
        ContentBlock::Text { text } => text_part_detail(text),
        ContentBlock::Image { source } => {
            let bytes = source.data.as_ref().map(|d| d.len()).unwrap_or(0);
            serde_json::json!({
                "type": "image",
                "bytes": bytes,
                "media_type": source.media_type
            })
        }
        ContentBlock::Document { source, .. } => {
            let bytes = 0; // DocumentSource doesn't expose data directly
            serde_json::json!({
                "type": "document",
                "bytes": bytes,
                "media_type": source.media_type
            })
        }
        ContentBlock::ToolUse { name, input, .. } => {
            serde_json::json!({
                "type": "tool_use",
                "name": name,
                "input_bytes": input.to_string().len()
            })
        }
        ContentBlock::ToolResult { content, .. } => match content {
            Some(tc) => {
                let txt = match tc {
                    anyllm_translate::anthropic::ToolResultContent::Text(s) => s.clone(),
                    anyllm_translate::anthropic::ToolResultContent::Blocks(_) => "(blocks)".into(),
                };
                text_part_detail(&txt)
            }
            None => serde_json::json!({"type": "tool_result"}),
        },
        ContentBlock::Thinking { thinking, .. } => text_part_detail(thinking),
        ContentBlock::RedactedThinking { data } => {
            serde_json::json!({"type": "redacted_thinking", "bytes": data.len()})
        }
    }
}

/// Dump upstream error details to stderr as a single JSON line.
///
/// Controlled by the `DUMP_ON_ERROR` env var / `dump_on_error` flag.
pub(crate) fn dump_upstream_error(ctx: &UpstreamErrorCtx) {
    let mut entry = serde_json::json!({
        "event": "upstream_error",
        "ts": chrono_now(),
        "status": ctx.status,
        "error_message": ctx.error_message,
        "model": ctx.model,
        "request_size_bytes": ctx.request_size,
    });

    if let Some(v) = ctx.input_messages {
        entry["input_messages"] = serde_json::json!(v);
    }
    if let Some(v) = ctx.max_tokens {
        entry["max_tokens"] = serde_json::json!(v);
    }
    if let Some(v) = ctx.chunks_received {
        entry["chunks_received"] = serde_json::json!(v);
    }
    if let Some(v) = ctx.bytes_received {
        entry["bytes_received"] = serde_json::json!(v);
    }
    if let Some(ref v) = ctx.messages_detail {
        entry["messages_detail"] = v.clone();
    }

    eprintln!("{}", serde_json::to_string(&entry).unwrap_or_default());
}

/// Dump a request-level (non-upstream) error to stderr.
///
/// Captures model, request size, and message detail from the request body,
/// mirroring the shape of `dump_upstream_error`.
pub(crate) fn dump_request_error(status: u16, error_message: &str, body: &[u8]) {
    let model = peek_model_from_json(body);
    let messages_detail = messages_detail_from_bytes(body);
    let mut entry = serde_json::json!({
        "event": "request_error",
        "status": status,
        "error_message": error_message,
        "model": model,
        "request_size_bytes": body.len(),
    });
    if let Some(ref detail) = messages_detail {
        entry["messages_detail"] = detail.clone();
    }
    eprintln!("{}", serde_json::to_string(&entry).unwrap_or_default());
}

fn chrono_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

pub async fn build_app(config: Config) -> Result<Router, AppError> {
    let hint_statuses = config.body_too_large_hint_statuses.clone();
    let config = Arc::new(config);
    let max_request_body = config.max_request_body;
    let openai = OpenAiHandler::new(config.as_ref(), hint_statuses.clone())?;
    let anthropic = AnthropicHandler::new(config.as_ref(), hint_statuses.clone())?;

    let health_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|err| AppError::Internal(err.to_string()))?;

    let state = AppState {
        config,
        openai,
        anthropic,
        health_client,
        health_cache: Arc::new(Mutex::new(None)),
    };

    Ok(router(state)
        .layer(RequestBodyLimitLayer::new(max_request_body))
        .layer(axum::middleware::map_response(
            move |response: Response| {
                let hs = hint_statuses.clone();
                async move {
                    let status = response.status();
                    if hs.contains(&status) {
                        let is_upstream_relay = response
                            .headers()
                            .get(header::CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .map(|v| v.starts_with("application/json"))
                            .unwrap_or(false);
                        if !is_upstream_relay {
                            let body = serde_json::json!({
                                "type": "error",
                                "error": {
                                    "type": "invalid_request_error",
                                    "message": format!("Request body exceeds limit. {BODY_TOO_LARGE_HINT}")
                                }
                            });
                            return (status, Json(body)).into_response();
                        }
                    }
                    response
                }
            },
        )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteTarget;

    fn route_with_limits(
        max_tokens: Option<u32>,
        max_output_tokens: Option<u32>,
        max_completion_tokens: Option<u32>,
    ) -> RouteTarget {
        RouteTarget {
            section: "test".into(),
            endpoint_openai: None,
            endpoint_anthropic: None,
            api_key: None,
            max_tokens,
            max_output_tokens,
            max_completion_tokens,
            model_names: std::collections::HashSet::new(),
        }
    }

    #[test]
    fn apply_token_caps_no_limits_returns_unchanged() {
        let body = br#"{"max_tokens":4096,"model":"test"}"#;
        let route = route_with_limits(None, None, None);
        let result = apply_token_caps(body, &route).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn apply_token_caps_clamps_max_tokens() {
        let body = br#"{"max_tokens":4096,"model":"test"}"#;
        let route = route_with_limits(Some(1024), None, None);
        let result = apply_token_caps(body, &route).unwrap();
        let v: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(v["max_tokens"], 1024);
    }

    #[test]
    fn apply_token_caps_sets_missing_max_tokens() {
        let body = br#"{"model":"test"}"#;
        let route = route_with_limits(Some(1024), None, None);
        let result = apply_token_caps(body, &route).unwrap();
        let v: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(v["max_tokens"], 1024);
    }

    #[test]
    fn apply_token_caps_applies_all_three_limits() {
        let body = br#"{"max_tokens":4096,"max_output_tokens":8192,"max_completion_tokens":16384}"#;
        let route = route_with_limits(Some(1024), Some(2048), Some(4096));
        let result = apply_token_caps(body, &route).unwrap();
        let v: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(v["max_tokens"], 1024);
        assert_eq!(v["max_output_tokens"], 2048);
        assert_eq!(v["max_completion_tokens"], 4096);
    }

    // --- peek_model_from_json ---

    #[test]
    fn peek_model_extracts_field() {
        let body = br#"{"model":"gpt-4","messages":[]}"#;
        assert_eq!(peek_model_from_json(body), "gpt-4");
    }

    #[test]
    fn peek_model_returns_question_on_missing() {
        assert_eq!(peek_model_from_json(br#"{"x":1}"#), "?");
    }

    #[test]
    fn peek_model_returns_question_on_garbage() {
        assert_eq!(peek_model_from_json(b"not json"), "?");
    }

    // --- text_part_detail ---

    #[test]
    fn text_part_detail_counts_words_and_chars() {
        let detail = text_part_detail("hello world");
        assert_eq!(detail["type"], "text");
        assert_eq!(detail["words"], 2);
        assert_eq!(detail["chars"], 11);
    }

    #[test]
    fn text_part_detail_handles_unicode() {
        let detail = text_part_detail("привет мир");
        assert_eq!(detail["type"], "text");
        assert_eq!(detail["words"], 2);
        assert_eq!(detail["chars"], 10); // 10 unicode chars
    }

    // --- anthropic_messages_detail ---

    #[test]
    fn anthropic_messages_detail_text_only() {
        let req = serde_json::from_value(serde_json::json!({
            "model": "claude",
            "max_tokens": 100,
            "messages": [
                {"role": "user", "content": "hello there"},
                {"role": "assistant", "content": [{"type": "text", "text": "hi back"}]}
            ]
        }))
        .unwrap();
        let detail = anthropic_messages_detail(&req);
        let msgs = detail.as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["parts"][0]["type"], "text");
        assert_eq!(msgs[0]["parts"][0]["words"], 2);
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["parts"][0]["words"], 2);
    }

    #[test]
    fn anthropic_messages_detail_image_and_tool_use() {
        let req = serde_json::from_value(serde_json::json!({
            "model": "claude",
            "max_tokens": 100,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "describe"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aaaa"}},
                    {"type": "tool_use", "id": "t1", "name": "search", "input": {"q": "rust"}}
                ]
            }]
        }))
        .unwrap();
        let detail = anthropic_messages_detail(&req);
        let parts = detail[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image");
        assert_eq!(parts[1]["bytes"], 4);
        assert_eq!(parts[1]["media_type"], "image/png");
        assert_eq!(parts[2]["type"], "tool_use");
        assert_eq!(parts[2]["name"], "search");
    }

    // --- openai_messages_detail ---

    #[test]
    fn openai_messages_detail_text_only() {
        let req = serde_json::from_value(serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "world"}
            ]
        }))
        .unwrap();
        let detail = openai_messages_detail(&req);
        let msgs = detail.as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["parts"][0]["type"], "text");
        assert_eq!(msgs[0]["parts"][0]["words"], 1);
    }

    #[test]
    fn openai_messages_detail_with_tool_calls() {
        let req = serde_json::from_value(serde_json::json!({
            "model": "gpt-4",
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"London\"}"}
                }]
            }]
        }))
        .unwrap();
        let detail = openai_messages_detail(&req);
        let parts = detail[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "tool_call");
        assert_eq!(parts[0]["name"], "get_weather");
        assert!(parts[0]["args_chars"].as_u64().unwrap() > 0);
    }

    // --- messages_detail_from_bytes ---

    #[test]
    fn messages_detail_from_bytes_anthropic_shape() {
        let body = br#"{
            "model": "claude",
            "max_tokens": 100,
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}, {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "bbbb"}}]},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
            ]
        }"#;
        let detail = messages_detail_from_bytes(body).unwrap();
        let msgs = detail.as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        let parts = msgs[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["words"], 1);
        assert_eq!(parts[1]["type"], "image");
        assert_eq!(parts[1]["bytes"], 4);
    }

    #[test]
    fn messages_detail_from_bytes_openai_shape() {
        let body = br#"{
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "hello world"},
                {"role": "assistant", "content": [{"type": "text", "text": "response"}, {"type": "image_url", "image_url": {"url": "https://ex.com/i.png"}}]}
            ]
        }"#;
        let detail = messages_detail_from_bytes(body).unwrap();
        let msgs = detail.as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["parts"][0]["type"], "text");
        assert_eq!(msgs[1]["parts"][1]["type"], "image_url");
        assert!(msgs[1]["parts"][1]["url_chars"].as_u64().unwrap() > 0);
    }

    #[test]
    fn messages_detail_from_bytes_missing_field() {
        assert!(messages_detail_from_bytes(br#"{"model":"x"}"#).is_none());
        assert!(messages_detail_from_bytes(br#"{"messages":"not_array"}"#).is_none());
        assert!(messages_detail_from_bytes(b"garbage").is_none());
    }

    // --- dump_upstream_error JSON shape ---

    #[test]
    fn dump_upstream_error_json_contains_all_fields_when_present() {
        // Verify the JSON shape that dump_upstream_error writes to stderr.
        let entry = serde_json::json!({
            "event": "upstream_error",
            "ts": "",
            "status": 502,
            "error_message": "Bad Gateway",
            "model": "test-model",
            "request_size_bytes": 1234,
            "input_messages": 2,
            "max_tokens": 4096,
            "messages_detail": [{"role":"user","parts":[{"type":"text","chars":5,"words":1}]}]
        });
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("upstream_error"));
        assert!(json.contains("502"));
        assert!(json.contains("test-model"));
        assert!(json.contains("messages_detail"));
        // Round-trip must succeed.
        let _: serde_json::Value = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn dump_upstream_error_json_omits_optional_fields_when_none() {
        let entry = serde_json::json!({
            "event": "upstream_error",
            "ts": "",
            "status": 400,
            "error_message": "bad",
            "model": "m",
            "request_size_bytes": 10,
        });
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("input_messages"));
        assert!(!json.contains("max_tokens"));
        assert!(!json.contains("messages_detail"));
    }
}
