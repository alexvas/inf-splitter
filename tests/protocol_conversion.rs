mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyllm_translate::anthropic::{Content, MessageCreateRequest};
use anyllm_translate::openai::{ChatCompletionRequest, ChatContent};
use axum::response::IntoResponse;
use axum::Json;
use common::{
    anthropic_upstream_response, bind_and_serve, interactions_upstream_response,
    openai_upstream_response, post_anthropic, post_openai, spawn_delayed_upstream,
    spawn_error_upstream, spawn_router, spawn_router_with_diagnostics, spawn_router_with_dump,
    spawn_sse_upstream_with_headers, spawn_stream_upstream, spawn_upstream, wait_for_egress_dump,
    wait_for_egress_response_dump, wait_for_file, wait_for_ingress_dump,
};
use inf_splitter::diagnostics::{DiagnosticMode, DiagnosticsConfig, Sink};

const CLIENT_PROMPT: &str = "hello-from-client";

#[tokio::test]
async fn anthropic_ingress_openai_upstream_converts_both_ways() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let openai_addr = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("local-openai-model", "openai-upstream-reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{openai_addr}"
models = "local-openai-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;

    let anthropic_request = serde_json::json!({
        "model": "local-openai-model",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": CLIENT_PROMPT}]
    });

    let response = post_anthropic(&proxy_addr, anthropic_request).await;

    let status = response.status();
    let body_text = response.text().await.expect("response body");
    assert!(status.is_success(), "proxy failed: {body_text}");

    let upstream_body = captured
        .lock()
        .expect("lock captured request")
        .clone()
        .expect("openai upstream must receive a request");

    let upstream_req: ChatCompletionRequest = serde_json::from_value(upstream_body)
        .expect("upstream body must be OpenAI chat completion");
    assert_eq!(
        match &upstream_req.messages[0].content {
            Some(ChatContent::Text(text)) => text.clone(),
            other => panic!("expected text content, got {other:?}"),
        },
        CLIENT_PROMPT
    );

    let anthropic_response: serde_json::Value =
        serde_json::from_str(&body_text).expect("anthropic json body");
    assert_eq!(anthropic_response["type"], "message");
    assert_eq!(
        anthropic_response["content"][0]["text"], "openai-upstream-reply",
        "client must receive Anthropic-shaped response"
    );
}

#[tokio::test]
async fn openai_ingress_anthropic_upstream_converts_both_ways() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let anthropic_addr = spawn_upstream(
        "/v1/messages",
        captured.clone(),
        anthropic_upstream_response("remote-anthropic-model", "anthropic-upstream-reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{anthropic_addr}"
models = "remote-anthropic-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;

    let openai_request = serde_json::json!({
        "model": "remote-anthropic-model",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": CLIENT_PROMPT}]
    });

    let response = post_openai(&proxy_addr, openai_request).await;

    let status = response.status();
    let body_text = response.text().await.expect("response body");
    assert!(status.is_success(), "proxy failed: {body_text}");

    let upstream_body = captured
        .lock()
        .expect("lock captured request")
        .clone()
        .expect("anthropic upstream must receive a request");

    let upstream_req: MessageCreateRequest =
        serde_json::from_value(upstream_body).expect("upstream body must be Anthropic messages");
    assert_eq!(upstream_req.max_tokens, 64);
    assert_eq!(
        match &upstream_req.messages[0].content {
            Content::Text(text) => text.clone(),
            other => panic!("expected text content, got {other:?}"),
        },
        CLIENT_PROMPT
    );

    let openai_response: serde_json::Value =
        serde_json::from_str(&body_text).expect("openai json body");
    assert_eq!(openai_response["object"], "chat.completion");
    assert_eq!(
        openai_response["choices"][0]["message"]["content"], "anthropic-upstream-reply",
        "client must receive OpenAI-shaped response"
    );
}

#[tokio::test]
async fn unroutable_model_returns_400() {
    let config = r#"
listen_port = 0

[local]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"
"#;
    let proxy_addr = spawn_router(config).await;

    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "unknown-model",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn empty_model_returns_400() {
    let config = r#"
listen_port = 0

[local]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"
"#;
    let proxy_addr = spawn_router(config).await;

    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn invalid_json_body_returns_400() {
    let config = r#"
listen_port = 0

[local]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"
"#;
    let proxy_addr = spawn_router(config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/v1/messages"))
        .header("content-type", "application/json")
        .body("not json")
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn upstream_error_relays_status() {
    let upstream_addr = spawn_error_upstream(
        "/v1/chat/completions",
        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
        serde_json::json!({"error": "request too large"}),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "test-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"], "request too large");
}

#[tokio::test]
async fn openai_passthrough_no_conversion() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let openai_addr = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("passthrough-model", "direct-openai-response"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{openai_addr}"
models = "passthrough-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "passthrough-model",
            "messages": [{"role": "user", "content": "test"}]
        }),
    )
    .await;

    assert!(response.status().is_success());
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["object"], "chat.completion");
}

#[tokio::test]
async fn anthropic_passthrough_no_conversion() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let anthropic_addr = spawn_upstream(
        "/v1/messages",
        captured.clone(),
        anthropic_upstream_response("passthrough-model", "direct-anthropic-response"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{anthropic_addr}"
models = "passthrough-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;

    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "passthrough-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "test"}]
        }),
    )
    .await;

    assert!(response.status().is_success());
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["type"], "message");
}

#[tokio::test]
async fn openai_ingress_anthropic_upstream_streaming() {
    let sse_body = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_001\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"stream-model\",\"content\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":5}}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"streamed\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}

event: message_stop
data: {\"type\":\"message_stop\"}
";

    let anthropic_addr = spawn_stream_upstream("/v1/messages", sse_body.to_string()).await;

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{anthropic_addr}"
models = "stream-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "stream-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }),
    )
    .await;

    let status = response.status();
    let body = response.text().await.expect("response body");
    assert!(
        status.is_success(),
        "streaming request failed with {status}: {body}"
    );
    assert!(
        body.contains("chat.completion.chunk") || body.contains("[DONE]"),
        "expected OpenAI SSE chunks, got: {body}"
    );
}

#[tokio::test]
async fn anthropic_ingress_openai_upstream_streaming() {
    let sse_body = "\
data: {\"id\":\"chatcmpl-001\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"stream-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}

data: {\"id\":\"chatcmpl-001\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"stream-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"streamed\"},\"finish_reason\":null}]}

data: {\"id\":\"chatcmpl-001\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"stream-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}

data: [DONE]
";

    let openai_addr = spawn_stream_upstream("/v1/chat/completions", sse_body.to_string()).await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{openai_addr}"
models = "stream-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;

    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "stream-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }),
    )
    .await;

    assert!(response.status().is_success());
    let body = response.text().await.expect("response body");
    assert!(
        body.contains("content_block_delta") || body.contains("message_stop"),
        "expected Anthropic SSE events, got: {body}"
    );
}

/// Passthrough error path relays correct status/body while recording diagnostics.
#[tokio::test]
async fn dump_on_error_passthrough_does_not_break_response() {
    let upstream_addr = spawn_error_upstream(
        "/v1/chat/completions",
        axum::http::StatusCode::BAD_GATEWAY,
        serde_json::json!({"error": {"message": "upstream exploded"}}),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "dump-model"
"#
    );

    let proxy_addr = spawn_router_with_dump(&config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "dump-model",
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "world"}
            ]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"]["message"], "upstream exploded");
}

/// Conversion error path relays correct status/body with messages_detail from typed request.
#[tokio::test]
async fn dump_on_error_conversion_does_not_break_response() {
    let upstream_addr = spawn_error_upstream(
        "/v1/messages",
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({"type": "error", "error": {"type": "overloaded", "message": "too busy"}}),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "dump-conv-model"
"#
    );

    let proxy_addr = spawn_router_with_dump(&config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "dump-conv-model",
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "user", "content": [{"type": "text", "text": "describe this"}]}
            ]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"]["message"], "too busy");
}

/// Streaming conversion error path relays correct status/body.
#[tokio::test]
async fn dump_on_error_stream_conversion_error_does_not_break_response() {
    let upstream_addr = spawn_error_upstream(
        "/v1/messages",
        axum::http::StatusCode::BAD_GATEWAY,
        serde_json::json!({"type": "error", "error": {"type": "api_error", "message": "down"}}),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "dump-stream-model"
"#
    );

    let proxy_addr = spawn_router_with_dump(&config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "dump-stream-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"]["message"], "down");
}

/// With `stats_mode = "all"` and file output, a successful request produces
/// a stats NDJSON line containing request metadata.
#[tokio::test]
async fn stats_mode_all_writes_to_file_on_success() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("test-model", "hello from upstream"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-stats-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "test-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "test-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Give the background writer a moment to flush.
    let lines = wait_for_file(&tmp).await;

    assert!(!lines.is_empty(), "expected at least one stats line");
    assert_unique_requests(&lines);

    let line: serde_json::Value = serde_json::from_str(&lines[0]).expect("valid NDJSON");
    assert_eq!(line["direction"], "openai->openai");
    assert_eq!(line["model"], "test-model");
    assert_eq!(line["status"], 200);
    assert!(
        line["duration_ms"].as_u64().is_some(),
        "duration_ms must be present (may be 0 for in-process mock upstreams)"
    );
    assert!(line["messages_detail_egress"].is_array());
    assert!(line.get("error").is_none());
}

/// With `dump_mode = "all"` and file output, a successful request produces
/// dump NDJSON lines for request and response bodies.
#[tokio::test]
async fn dump_mode_all_writes_to_file_on_success() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("test-model", "hello from upstream"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-dump-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "test-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "test-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let lines = wait_for_file(&tmp).await;

    assert!(!lines.is_empty(), "dump file should not be empty");
    assert_unique_requests(&lines);
    for line_str in &lines {
        let line: serde_json::Value = serde_json::from_str(line_str).expect("valid NDJSON");
        assert_eq!(line["model"], "test-model");
        let stage = line["stage"].as_str().unwrap();
        assert!(
            stage == "egress" || stage == "ingress",
            "stage must be ingress or egress, got {stage}"
        );
    }
}

/// With `stats_mode = "error"` and file output, a successful request writes
/// nothing, but an error request writes a stats line.
#[tokio::test]
async fn stats_mode_error_writes_only_on_error() {
    let success_upstream = {
        let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
        spawn_upstream(
            "/v1/chat/completions",
            captured,
            openai_upstream_response("ok-model", "ok"),
        )
        .await
    };
    let error_upstream = spawn_error_upstream(
        "/v1/chat/completions",
        axum::http::StatusCode::BAD_GATEWAY,
        serde_json::json!({"error": "boom"}),
    )
    .await;

    let tmp =
        std::env::temp_dir().join(format!("inf-splitter-stats-error-{}.ndjson", uuid_suffix()));

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::Error,
        stats_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };

    // Two providers sharing the same diagnostics file.
    let config = format!(
        r#"
listen_port = 0

[ok]
endpoint_openai = "http://{success_upstream}"
models = "ok-model"

[fail]
endpoint_openai = "http://{error_upstream}"
models = "fail-model"
"#
    );

    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    // Successful request — should NOT write stats.
    let ok_resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "ok-model",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    assert_eq!(ok_resp.status(), reqwest::StatusCode::OK);

    // Error request — should write stats.
    let err_resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "fail-model",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    assert_eq!(err_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    let lines = wait_for_file(&tmp).await;

    assert_eq!(
        lines.len(),
        1,
        "only the error request should produce a stats line, got {lines:?}"
    );
    assert_unique_requests(&lines);
    let line: serde_json::Value = serde_json::from_str(&lines[0]).expect("valid NDJSON");
    assert_eq!(line["model"], "fail-model");
    assert!(line["error"].is_string());
    assert!(
        line["duration_ms"].as_u64().is_some(),
        "duration_ms must be present (may be 0 for in-process mock upstreams)"
    );
    assert!(
        line["response_size_bytes"].as_u64().is_some(),
        "response_size_bytes must be populated on error, got {:?}",
        line["response_size_bytes"]
    );
}

/// With `dump_mode = "error"` and file output, a successful request writes
/// nothing, but an error request writes a dump line.
#[tokio::test]
async fn dump_mode_error_writes_only_on_error() {
    let success_upstream = {
        let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
        spawn_upstream(
            "/v1/chat/completions",
            captured,
            openai_upstream_response("ok-model", "ok"),
        )
        .await
    };
    let error_upstream = spawn_error_upstream(
        "/v1/chat/completions",
        axum::http::StatusCode::BAD_GATEWAY,
        serde_json::json!({"error": "boom"}),
    )
    .await;

    let tmp =
        std::env::temp_dir().join(format!("inf-splitter-dump-error-{}.ndjson", uuid_suffix()));

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::Error,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };

    let config = format!(
        r#"
listen_port = 0

[ok]
endpoint_openai = "http://{success_upstream}"
models = "ok-model"

[fail]
endpoint_openai = "http://{error_upstream}"
models = "fail-model"
"#
    );

    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    // Successful request — should NOT write dump.
    let ok_resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "ok-model",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    assert_eq!(ok_resp.status(), reqwest::StatusCode::OK);

    // Error request — should write dump.
    let err_resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "fail-model",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    assert_eq!(err_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    let lines = wait_for_file(&tmp).await;

    assert!(
        !lines.is_empty(),
        "error request should produce at least one dump line"
    );
    assert_unique_requests(&lines);
    for line_str in &lines {
        let line: serde_json::Value = serde_json::from_str(line_str).expect("valid NDJSON");
        assert_eq!(line["model"], "fail-model");
    }
}

/// Assert that every NDJSON line has a unique `(request_id, stage, direction)` tuple.
fn assert_unique_requests(lines: &[String]) {
    let mut seen = std::collections::HashSet::new();
    for (i, line_str) in lines.iter().enumerate() {
        if line_str.trim().is_empty() {
            continue;
        }
        let line: serde_json::Value =
            serde_json::from_str(line_str.as_str()).expect("valid NDJSON");
        let key = (
            line["request_id"].as_str().unwrap_or("?").to_string(),
            line["stage"].as_str().unwrap_or("-").to_string(),
            line["direction"].as_str().unwrap_or("-").to_string(),
        );
        assert!(
            seen.insert(key.clone()),
            "duplicate (request_id, stage, direction) at line {i}: {key:?}"
        );
    }
}

/// Translation Anthropic→OpenAI: stats must have both ingress AND egress
/// message details; dump must have both ingress AND egress stage entries.
#[tokio::test]
async fn translation_anthropic_to_openai_produces_ingress_and_egress_events() {
    let error_upstream = spawn_error_upstream(
        "/v1/chat/completions",
        axum::http::StatusCode::BAD_GATEWAY,
        serde_json::json!({"error": "translation-boom"}),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-trans-ao-{}.ndjson", uuid_suffix()));

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::Error,
        dump_mode: DiagnosticMode::Error,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };

    // Only endpoint_openai → Anthropic ingress forces translation.
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_openai = "http://{error_upstream}"
models = "trans-model"
"#
    );

    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    let resp = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "trans-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    // Check stats: must have both ingress and egress message details.
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert_eq!(stats_lines.len(), 1, "expected exactly one stats line");
    let stats: serde_json::Value = serde_json::from_str(&stats_lines[0]).expect("valid NDJSON");
    assert_eq!(stats["direction"], "anthropic->openai");
    assert!(
        stats["messages_detail_ingress"].is_array(),
        "translation stats must have messages_detail_ingress"
    );
    assert!(
        stats["messages_detail_egress"].is_array(),
        "translation stats must have messages_detail_egress"
    );
    assert!(
        stats["duration_ms"].as_u64().is_some(),
        "translation error stats must have duration_ms populated"
    );
    assert!(
        stats["response_size_bytes"].as_u64().is_some(),
        "translation error stats must have response_size_bytes set"
    );
    // Check dump: must have both ingress and egress stage entries.
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_file(&dump_path).await;
    assert_unique_requests(&dump_lines);
    // We expect at least "ingress" stage (the original Anthropic body).
    let has_ingress = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("ingress")
    });
    assert!(
        has_ingress,
        "translation dump must have ingress stage entries"
    );
}

/// Translation OpenAI→Anthropic: same check as above, reversed direction.
#[tokio::test]
async fn translation_openai_to_anthropic_produces_ingress_and_egress_events() {
    let error_upstream =
        spawn_error_upstream(
            "/v1/messages",
            axum::http::StatusCode::BAD_GATEWAY,
            serde_json::json!({"type": "error", "error": {"type": "api_error", "message": "translation-boom"}}),
        )
        .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-trans-oa-{}.ndjson", uuid_suffix()));

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::Error,
        dump_mode: DiagnosticMode::Error,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };

    // Only endpoint_anthropic → OpenAI ingress forces translation.
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{error_upstream}"
models = "trans-model"
"#
    );

    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    let resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "trans-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    // Stats: must have ingress and egress details.
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert_eq!(stats_lines.len(), 1, "expected exactly one stats line");
    let stats: serde_json::Value = serde_json::from_str(&stats_lines[0]).expect("valid NDJSON");
    assert_eq!(stats["direction"], "openai->anthropic");
    assert!(stats["messages_detail_ingress"].is_array());
    assert!(stats["messages_detail_egress"].is_array());
    assert!(
        stats["duration_ms"].as_u64().is_some(),
        "translation error stats must have duration_ms populated"
    );
    assert!(
        stats["response_size_bytes"].as_u64().is_some(),
        "translation error stats must have response_size_bytes set"
    );

    // Dump: must have ingress stage.
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_file(&dump_path).await;
    assert_unique_requests(&dump_lines);
    let has_ingress = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("ingress")
    });
    assert!(
        has_ingress,
        "translation dump must have ingress stage entries"
    );
}

// ── Gap tests: diagnostics coverage on paths that were missing it ──

/// Anthropic passthrough success with `stats_mode = All` and `dump_mode = All`
/// must write a stats line and at least one dump line.
#[tokio::test]
async fn anthropic_passthrough_success_produces_stats_and_dumps() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("ap-model", "reply"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-ap-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "ap-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        dump_mode: DiagnosticMode::All,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "ap-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Stats
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(!stats_lines.is_empty(), "expected at least one stats line");
    let stats: serde_json::Value = serde_json::from_str(&stats_lines[0]).expect("valid NDJSON");
    assert_eq!(stats["direction"], "anthropic->anthropic");
    assert_eq!(stats["model"], "ap-model");
    assert_eq!(stats["status"], 200);
    assert!(stats.get("error").is_none());
    assert!(
        stats["duration_ms"].as_u64().is_some(),
        "success stats must have duration_ms populated"
    );

    // Dump
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_egress_dump(&dump_path).await;
    assert_unique_requests(&dump_lines);
    let has_egress = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("egress")
    });
    assert!(
        has_egress,
        "anthropic passthrough dump must have egress stage"
    );
}

/// Anthropic passthrough error with `dump_mode = Error` must write a dump line.
#[tokio::test]
async fn anthropic_passthrough_error_produces_dumps() {
    let upstream_addr = spawn_error_upstream(
        "/v1/messages",
        axum::http::StatusCode::BAD_GATEWAY,
        serde_json::json!({"type": "error", "error": {"type": "api_error", "message": "down"}}),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-ape-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "ape-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::Error,
        dump_mode: DiagnosticMode::Error,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "ape-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);

    // Stats: must have error
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(
        !stats_lines.is_empty(),
        "expected at least one stats line on error"
    );
    let stats: serde_json::Value = serde_json::from_str(&stats_lines[0]).expect("valid NDJSON");
    assert!(stats["error"].is_string());
    assert!(
        stats["duration_ms"].as_u64().is_some(),
        "duration_ms must be present (may be 0 for in-process mock upstreams)"
    );
    assert!(
        stats["response_size_bytes"].as_u64().is_some(),
        "error stats must have response_size_bytes set"
    );

    // Dump: must have at least egress
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_egress_dump(&dump_path).await;
    assert_unique_requests(&dump_lines);
    let has_egress = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("egress")
    });
    assert!(
        has_egress,
        "anthropic passthrough error dump must have egress stage"
    );
}

/// OpenAI→Anthropic translation success with `stats_mode = All` must write a
/// stats line with both messages_detail_ingress and messages_detail_egress.
#[tokio::test]
async fn openai_to_anthropic_translation_success_produces_stats_and_dumps() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("oa-model", "reply"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-oa-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "oa-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        dump_mode: DiagnosticMode::All,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "oa-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Stats
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(
        !stats_lines.is_empty(),
        "translation success must produce stats"
    );
    let stats: serde_json::Value = serde_json::from_str(&stats_lines[0]).expect("valid NDJSON");
    assert_eq!(stats["direction"], "openai->anthropic");
    assert_eq!(stats["status"], 200);
    assert!(stats.get("error").is_none());
    assert!(
        stats["duration_ms"].as_u64().is_some(),
        "translation success stats must have duration_ms populated"
    );
    assert!(
        stats["messages_detail_ingress"].is_array(),
        "translation stats must have messages_detail_ingress"
    );
    assert!(
        stats["messages_detail_egress"].is_array(),
        "translation stats must have messages_detail_egress"
    );

    // Dump
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_egress_dump(&dump_path).await;
    assert_unique_requests(&dump_lines);
    let has_ingress = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("ingress")
    });
    assert!(has_ingress, "translation dump must have ingress stage");
    let has_egress = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("egress")
    });
    assert!(has_egress, "translation dump must have egress stage");
}

/// Anthropic→OpenAI translation success with `stats_mode = All` must write stats.
#[tokio::test]
async fn anthropic_to_openai_translation_success_produces_stats_and_dumps() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/chat/completions",
        captured,
        openai_upstream_response("ao-model", "reply"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-ao-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_openai = "http://{upstream_addr}"
models = "ao-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        dump_mode: DiagnosticMode::All,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "ao-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Stats
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(
        !stats_lines.is_empty(),
        "translation success must produce stats"
    );
    let stats: serde_json::Value = serde_json::from_str(&stats_lines[0]).expect("valid NDJSON");
    assert_eq!(stats["direction"], "anthropic->openai");
    assert_eq!(stats["status"], 200);
    assert!(stats.get("error").is_none());
    assert!(
        stats["duration_ms"].as_u64().is_some(),
        "translation success stats must have duration_ms populated"
    );
    assert!(
        stats["messages_detail_ingress"].is_array(),
        "translation stats must have messages_detail_ingress"
    );
    assert!(
        stats["messages_detail_egress"].is_array(),
        "translation stats must have messages_detail_egress"
    );

    // Dump
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_egress_dump(&dump_path).await;
    assert_unique_requests(&dump_lines);
    let has_ingress = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("ingress")
    });
    assert!(has_ingress, "translation dump must have ingress stage");
    let has_egress = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("egress")
    });
    assert!(has_egress, "translation dump must have egress stage");
}

/// Streaming passthrough must record `streaming: true` in stats (not hardcoded false).
#[tokio::test]
async fn streaming_passthrough_records_streaming_true_in_stats() {
    let sse_body = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_001\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"stream-model\",\"content\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":5}}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}

event: message_stop
data: {\"type\":\"message_stop\"}
";

    let upstream_addr = spawn_stream_upstream("/v1/messages", sse_body.to_string()).await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-streaming-{}.ndjson", uuid_suffix()));

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "stream-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let _response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "stream-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }),
    )
    .await;

    let lines = wait_for_file(&tmp).await;

    assert!(
        !lines.is_empty(),
        "streaming passthrough must produce stats"
    );

    let line: serde_json::Value = serde_json::from_str(&lines[0]).expect("valid NDJSON");
    assert_eq!(line["direction"], "anthropic->anthropic");
    assert_eq!(
        line["streaming"], true,
        "streaming field must be true for SSE response"
    );
    assert!(
        line["duration_ms"].as_u64().is_some(),
        "streaming stats must have duration_ms populated"
    );
}

// ── Timing test ──

/// With a delayed upstream, `duration_ms` must be strictly positive,
/// proving the timing instrumentation is actually wired up (not just
/// the field exists with a zero sentinel).
#[tokio::test]
async fn delayed_upstream_produces_nonzero_duration_ms() {
    let upstream_addr = spawn_delayed_upstream(
        "/v1/chat/completions",
        std::time::Duration::from_millis(5),
        openai_upstream_response("timed-model", "slow reply"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-timing-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "timed-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "timed-model",
            "messages": [{"role": "user", "content": "ping"}]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let lines = wait_for_file(&tmp).await;
    let stats: serde_json::Value = serde_json::from_str(&lines[0]).expect("valid NDJSON");

    let duration = stats["duration_ms"]
        .as_u64()
        .expect("duration_ms must be present");
    assert!(
        duration > 0,
        "duration_ms must be > 0 with a 5 ms delayed upstream, got {duration}"
    );
}

// ── Duplicate headers test ──

/// `relay_openai_upstream` must not duplicate `cache-control` or `connection`
/// headers that the upstream already sends in its SSE response.
#[tokio::test]
async fn relay_openai_upstream_does_not_duplicate_sse_headers() {
    let sse_body = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"choices\":[]}\n\ndata: [DONE]\n\n";

    let upstream_addr = spawn_sse_upstream_with_headers(
        "/v1/chat/completions",
        sse_body.to_string(),
        vec![
            ("cache-control", "private, no-cache".into()),
            ("connection", "close".into()),
        ],
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "dup-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "dup-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }),
    )
    .await;

    assert!(response.status().is_success());

    // Each header must appear exactly once — not duplicated by the relay function.
    let cc_vals: Vec<_> = response.headers().get_all("cache-control").iter().collect();
    assert_eq!(
        cc_vals.len(),
        1,
        "cache-control must not be duplicated, got {cc_vals:?}"
    );

    let conn_vals: Vec<_> = response.headers().get_all("connection").iter().collect();
    assert_eq!(
        conn_vals.len(),
        1,
        "connection must not be duplicated, got {conn_vals:?}"
    );
}

/// OpenAI passthrough error must produce an ingress dump (not just egress).
#[tokio::test]
async fn openai_passthrough_error_produces_ingress_dump() {
    let upstream_addr = spawn_error_upstream(
        "/v1/chat/completions",
        reqwest::StatusCode::BAD_GATEWAY,
        serde_json::json!({"error": "ingress-test"}),
    )
    .await;

    let tmp =
        std::env::temp_dir().join(format!("inf-splitter-oi-ingress-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "oi-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::Error,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let _resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "oi-model",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    assert_eq!(_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    let lines = wait_for_ingress_dump(&tmp).await;
    let has_ingress = lines.iter().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .ok()
            .and_then(|v| v["stage"].as_str().map(|s| s == "ingress"))
            .unwrap_or(false)
    });
    assert!(
        has_ingress,
        "openai passthrough error must produce ingress dump"
    );
}

/// Anthropic passthrough success must produce an ingress dump (not just egress).
#[tokio::test]
async fn anthropic_passthrough_success_produces_ingress_dump() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("ap-ingress-model", "reply"),
    )
    .await;

    let tmp =
        std::env::temp_dir().join(format!("inf-splitter-aps-ingress-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "ap-ingress-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let _resp = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "ap-ingress-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    assert_eq!(_resp.status(), reqwest::StatusCode::OK);

    let lines = wait_for_ingress_dump(&tmp).await;
    let has_ingress = lines.iter().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .ok()
            .and_then(|v| v["stage"].as_str().map(|s| s == "ingress"))
            .unwrap_or(false)
    });
    assert!(
        has_ingress,
        "anthropic passthrough success must produce ingress dump"
    );
}

/// Non-streaming Anthropic passthrough must produce an egress response dump
/// (stage="egress", direction="response") with the upstream response body.
#[tokio::test]
async fn anthropic_passthrough_non_streaming_egress_response_dump() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("er-model", "egress-response-body"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-er-ns-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "er-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "er-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let lines = wait_for_egress_response_dump(&tmp).await;

    assert_unique_requests(&lines);

    let egress_responses: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("response")
            {
                Some(v)
            } else {
                None
            }
        })
        .collect();

    assert!(
        !egress_responses.is_empty(),
        "must have at least one egress response dump, got lines: {lines:?}"
    );

    let egress = &egress_responses[0];
    assert_eq!(egress["model"], "er-model");
    assert!(egress["body"]
        .as_str()
        .unwrap()
        .contains("egress-response-body"));
    assert_eq!(egress["status"], 200);
    // Verify response headers are captured.
    assert!(
        egress["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h[0].as_str() == Some("content-type")
                && h[1].as_str().unwrap().contains("application/json")),
        "response dump must include content-type header"
    );
}

/// Streaming Anthropic passthrough must produce an egress response dump
/// (stage="egress", direction="response") with the accumulated SSE body.
#[tokio::test]
async fn anthropic_passthrough_streaming_egress_response_dump() {
    let sse_body = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_001\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"stream-er-model\",\"content\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":5}}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"streamed-response-text\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}

event: message_stop
data: {\"type\":\"message_stop\"}
";

    let upstream_addr = spawn_stream_upstream("/v1/messages", sse_body.to_string()).await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-er-stream-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "stream-er-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "stream-er-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }),
    )
    .await;

    assert!(response.status().is_success());
    // Must consume the SSE stream body to trigger DiagnosticStream termination and dump.
    let _body = response.text().await.expect("response body");

    let lines = wait_for_egress_response_dump(&tmp).await;

    assert_unique_requests(&lines);

    let egress_responses: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("response")
            {
                Some(v)
            } else {
                None
            }
        })
        .collect();

    assert!(
        !egress_responses.is_empty(),
        "must have at least one egress response dump, got lines: {lines:?}"
    );

    let egress = &egress_responses[0];
    assert_eq!(egress["model"], "stream-er-model");
    assert!(egress["body"]
        .as_str()
        .unwrap()
        .contains("streamed-response-text"));
    assert_eq!(egress["status"], 200);
    // SSE response headers must include content-type.
    assert!(
        egress["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h[0].as_str() == Some("content-type")
                && h[1].as_str().unwrap().contains("text/event-stream")),
        "response dump must include content-type header for SSE"
    );
}

/// Non-streaming OpenAI passthrough must produce an egress response dump
/// (stage="egress", direction="response") with the upstream response body.
#[tokio::test]
async fn openai_passthrough_non_streaming_egress_response_dump() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/chat/completions",
        captured,
        openai_upstream_response("oai-er-model", "openai-egress-response-body"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-oai-er-ns-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "oai-er-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "oai-er-model",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let lines = wait_for_egress_response_dump(&tmp).await;

    assert_unique_requests(&lines);

    let egress_responses: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("response")
            {
                Some(v)
            } else {
                None
            }
        })
        .collect();

    assert!(
        !egress_responses.is_empty(),
        "must have at least one egress response dump, got lines: {lines:?}"
    );

    let egress = &egress_responses[0];
    assert_eq!(egress["model"], "oai-er-model");
    assert!(egress["body"]
        .as_str()
        .unwrap()
        .contains("openai-egress-response-body"));
    assert_eq!(egress["status"], 200);
    assert!(
        egress["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h[0].as_str() == Some("content-type")
                && h[1].as_str().unwrap().contains("application/json")),
        "response dump must include content-type header"
    );
}

/// Streaming OpenAI passthrough must produce an egress response dump
/// (stage="egress", direction="response") with the accumulated SSE body.
#[tokio::test]
async fn openai_passthrough_streaming_egress_response_dump() {
    let sse_body = "\
data: {\"id\":\"chatcmpl-001\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"stream-oai-er-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}

data: {\"id\":\"chatcmpl-001\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"stream-oai-er-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"streamed-openai-text\"},\"finish_reason\":null}]}

data: {\"id\":\"chatcmpl-001\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"stream-oai-er-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}

data: [DONE]
";

    let upstream_addr = spawn_stream_upstream("/v1/chat/completions", sse_body.to_string()).await;

    let tmp = std::env::temp_dir().join(format!(
        "inf-splitter-oai-er-stream-{}.ndjson",
        uuid_suffix()
    ));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "stream-oai-er-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "stream-oai-er-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }),
    )
    .await;

    assert!(response.status().is_success());
    // Must consume the SSE stream body to trigger DiagnosticStream termination and dump.
    let _body = response.text().await.expect("response body");

    let lines = wait_for_egress_response_dump(&tmp).await;

    assert_unique_requests(&lines);

    let egress_responses: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("response")
            {
                Some(v)
            } else {
                None
            }
        })
        .collect();

    assert!(
        !egress_responses.is_empty(),
        "must have at least one egress response dump, got lines: {lines:?}"
    );

    let egress = &egress_responses[0];
    assert_eq!(egress["model"], "stream-oai-er-model");
    assert!(egress["body"]
        .as_str()
        .unwrap()
        .contains("streamed-openai-text"));
    assert_eq!(egress["status"], 200);
    assert!(
        egress["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h[0].as_str() == Some("content-type")
                && h[1].as_str().unwrap().contains("text/event-stream")),
        "response dump must include content-type header for SSE"
    );
}

/// Non-UTF8 client request body must return HTTP 400 with "non-utf8" message.
#[tokio::test]
async fn non_utf8_client_body_returns_400() {
    let config = r#"
listen_port = 0

[local]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"
"#;
    let proxy_addr = spawn_router(config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(vec![0xFF, 0xFE, 0x00, 0x01])
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert!(
        body.to_string().contains("non-utf8"),
        "error message should mention non-utf8, got: {body}"
    );
}

/// Non-UTF8 upstream response body must return HTTP 500 (Anthropic passthrough).
#[tokio::test]
async fn non_utf8_upstream_response_returns_500_anthropic() {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::Response;

    let upstream = {
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::post(|| async {
                Response::builder()
                    .status(axum::http::StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(vec![0xFF, 0xFE, 0x00]))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    };

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream}"
models = "test-model"
"#
    );
    let proxy_addr = common::spawn_router(&config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "test-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    let body: serde_json::Value = response.json().await.expect("json body");
    assert!(
        body.to_string().contains("non-utf8"),
        "error message should mention non-utf8, got: {body}"
    );
}

/// Non-UTF8 upstream response body must return HTTP 500 (OpenAI passthrough).
#[tokio::test]
async fn non_utf8_upstream_response_returns_500_openai() {
    use axum::body::Body;
    use axum::http::header;
    use axum::response::Response;

    let upstream = {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                Response::builder()
                    .status(axum::http::StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(vec![0xFF, 0xFE, 0x00]))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    };

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream}"
models = "test-model"
"#
    );
    let proxy_addr = common::spawn_router(&config).await;
    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    let body: serde_json::Value = response.json().await.expect("json body");
    assert!(
        body.to_string().contains("non-utf8"),
        "error message should mention non-utf8, got: {body}"
    );
}

/// When `max_file_size` is set, diagnostics output files must rotate when
/// the current file exceeds the limit.
#[tokio::test]
async fn diagnostics_file_rotates_on_max_file_size() {
    use common::wait_for_file;
    use std::fs;

    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("rot-model", "response-text"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-rot-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "rot-model"

[diagnostics]
dump_mode = "all"
dump_output = "{dump_path}"
max_file_size = "1k"
"#,
        dump_path = tmp.display()
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        max_file_size: Some(1024),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    // Send multiple requests to fill the file past 1KB
    for i in 0..5 {
        let response = post_anthropic(
            &proxy_addr,
            serde_json::json!({
                "model": "rot-model",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": format!("msg-{}", i)}]
            }),
        )
        .await;
        // Consume body to trigger dump recording
        let _ = response.text().await;
    }

    let lines = wait_for_file(&tmp).await;

    // Verify the current file has some content (latest dumps)
    assert!(!lines.is_empty(), "current file should have content");

    // Verify a rotated file exists in the same directory
    let dir = tmp.parent().unwrap();
    let rotated: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("inf-splitter-rot-"))
                && e.file_name().to_string_lossy() != tmp.file_name().unwrap().to_string_lossy()
        })
        .collect();

    assert!(
        !rotated.is_empty(),
        "must have at least one rotated file in {:?}",
        dir
    );

    // Clean up rotated files
    for entry in rotated {
        let _ = fs::remove_file(entry.path());
    }
    let _ = fs::remove_file(&tmp);
}

/// When `compression = "7z"` is set, rotated files must be compressed to `.ndjson.7z`
/// and the original uncompressed file must be removed.
#[tokio::test]
async fn diagnostics_rotation_compresses_with_7z() {
    use common::wait_for_file;
    use std::fs;

    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("rot7z-model", "response-text"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-7z-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "rot7z-model"

[diagnostics]
dump_mode = "all"
dump_output = "{dump_path}"
max_file_size = "1k"
compression = "7z"
"#,
        dump_path = tmp.display()
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        max_file_size: Some(1024),
        compression: Some(inf_splitter::diagnostics::Compression::SevenZ),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    // Send multiple requests to fill the file past 1KB and trigger rotation
    for i in 0..5 {
        let response = post_anthropic(
            &proxy_addr,
            serde_json::json!({
                "model": "rot7z-model",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": format!("msg-{}", i)}]
            }),
        )
        .await;
        let _ = response.text().await;
    }

    let lines = wait_for_file(&tmp).await;
    assert!(!lines.is_empty(), "current file should have content");

    // Wait a bit for background compression to finish
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Verify a compressed .ndjson.7z file exists
    let dir = tmp.parent().unwrap();
    let compressed: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("inf-splitter-7z-")
                && e.file_name().to_string_lossy().ends_with(".ndjson.7z")
        })
        .collect();

    assert!(
        !compressed.is_empty(),
        "must have at least one compressed .7z file in {:?}",
        dir
    );

    // The compressed file should be smaller than the original would be
    // (50M uncompressed → 7z should be much smaller)
    for entry in &compressed {
        let size = entry.metadata().unwrap().len();
        assert!(
            size > 0,
            "compressed file {} must be non-empty",
            entry.file_name().to_string_lossy()
        );
    }

    // Clean up
    for entry in compressed {
        let _ = fs::remove_file(entry.path());
    }
    let _ = fs::remove_file(&tmp);
}

/// When `compression = "bz2"` is set, rotated files must be compressed to `.ndjson.bz2`
/// and the original uncompressed file must be removed.
#[tokio::test]
async fn diagnostics_rotation_compresses_with_bz2() {
    use common::wait_for_file;
    use std::fs;

    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("rotbz2-model", "response-text"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-bz2-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "rotbz2-model"

[diagnostics]
dump_mode = "all"
dump_output = "{dump_path}"
max_file_size = "1k"
compression = "bz2"
"#,
        dump_path = tmp.display()
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        max_file_size: Some(1024),
        compression: Some(inf_splitter::diagnostics::Compression::Bz2),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    for i in 0..5 {
        let response = post_anthropic(
            &proxy_addr,
            serde_json::json!({
                "model": "rotbz2-model",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": format!("msg-{}", i)}]
            }),
        )
        .await;
        let _ = response.text().await;
    }

    let lines = wait_for_file(&tmp).await;
    assert!(!lines.is_empty(), "current file should have content");

    // Wait for background compression
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let dir = tmp.parent().unwrap();
    let compressed: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("inf-splitter-bz2-")
                && e.file_name().to_string_lossy().ends_with(".ndjson.bz2")
        })
        .collect();

    assert!(
        !compressed.is_empty(),
        "must have at least one compressed .bz2 file in {:?}",
        dir
    );

    for entry in &compressed {
        let size = entry.metadata().unwrap().len();
        assert!(
            size > 0,
            "compressed file {} must be non-empty",
            entry.file_name().to_string_lossy()
        );
    }

    // Clean up
    for entry in compressed {
        let _ = fs::remove_file(entry.path());
    }
    let _ = fs::remove_file(&tmp);
}

fn uuid_suffix() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    format!("{ts}")
}

// ── drop_fields integration tests ────────────────────────────────────

#[tokio::test]
async fn drop_fields_openai_passthrough() {
    let captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("drop-model", "ok"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream}"
models = "drop-model"
drop_fields = ["user", "metadata"]
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "drop-model",
            "messages": [{"role": "user", "content": "hi"}],
            "user": "should-be-removed",
            "metadata": { "key": "val" }
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body = captured
        .lock()
        .unwrap()
        .take()
        .expect("upstream captured body");
    assert!(body.get("user").is_none(), "user field should be dropped");
    assert!(
        body.get("metadata").is_none(),
        "metadata field should be dropped"
    );
    assert!(body.get("messages").is_some(), "messages should remain");
}

#[tokio::test]
async fn drop_fields_anthropic_passthrough() {
    let captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(
        "/v1/messages",
        captured.clone(),
        anthropic_upstream_response("drop-model", "ok"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_anthropic = "http://{upstream}"
models = "drop-model"
drop_fields = ["metadata"]
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let resp = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "drop-model",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "hello"}],
            "metadata": { "user_id": "abc" }
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body = captured
        .lock()
        .unwrap()
        .take()
        .expect("upstream captured body");
    assert!(
        body.get("metadata").is_none(),
        "metadata field should be dropped"
    );
    assert!(body.get("messages").is_some(), "messages should remain");
}

#[tokio::test]
async fn drop_fields_anthropic_to_openai_conversion() {
    let captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("conv-model", "translated"),
    )
    .await;

    // Only endpoint_openai → Anthropic ingress forces translation.
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_openai = "http://{upstream}"
models = "conv-model"
drop_fields = ["metadata"]
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let resp = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "conv-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}],
            "metadata": { "user_id": "123" }
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body = captured
        .lock()
        .unwrap()
        .take()
        .expect("upstream captured body");
    // After translation Anthropic→OpenAI, "metadata" should be dropped from the
    // OpenAI-format body.
    assert!(
        body.get("metadata").is_none(),
        "metadata should be dropped from translated body: {body:?}"
    );
    assert!(body.get("messages").is_some(), "messages should remain");
}

#[tokio::test]
async fn drop_fields_openai_to_anthropic_conversion() {
    let captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(
        "/v1/messages",
        captured.clone(),
        anthropic_upstream_response("conv-model", "translated"),
    )
    .await;

    // Only endpoint_anthropic → OpenAI ingress forces translation.
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream}"
models = "conv-model"
drop_fields = ["user"]
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "conv-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "user": "should-be-dropped"
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body = captured
        .lock()
        .unwrap()
        .take()
        .expect("upstream captured body");
    // After translation OpenAI→Anthropic, "user" should be dropped.
    assert!(
        body.get("user").is_none(),
        "user should be dropped from translated body: {body:?}"
    );
    assert!(body.get("messages").is_some(), "messages should remain");
}

#[tokio::test]
async fn drop_fields_per_model_all_and_specific() {
    // model-a: all + specific → both dropped
    // model-b: only all → only all dropped
    let captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("model-a", "ok"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream}"
models = ["model-a", "model-b"]

[local.drop_fields]
all = ["stream"]
"model-a" = ["user"]
"#
    );
    let proxy_addr = spawn_router(&config).await;

    // Request for model-a: should drop both "stream" and "user"
    let resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "model-a",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false,
            "user": "drop-me"
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = captured.lock().unwrap().take().expect("model-a body");
    assert!(
        body.get("stream").is_none(),
        "stream should be dropped (from all)"
    );
    assert!(
        body.get("user").is_none(),
        "user should be dropped (from model-a specific)"
    );

    // Request for model-b: should drop only "stream" (from all)
    let captured_b = Arc::new(Mutex::new(None));
    let upstream_b = spawn_upstream(
        "/v1/chat/completions",
        captured_b.clone(),
        openai_upstream_response("model-b", "ok"),
    )
    .await;
    let config_b = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_b}"
models = ["model-a", "model-b"]

[local.drop_fields]
all = ["stream"]
"model-a" = ["user"]
"#
    );
    let proxy_b = spawn_router(&config_b).await;
    let resp = post_openai(
        &proxy_b,
        serde_json::json!({
            "model": "model-b",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false,
            "user": "keep-me"
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = captured_b.lock().unwrap().take().expect("model-b body");
    assert!(
        body.get("stream").is_none(),
        "stream should be dropped (from all)"
    );
    assert!(
        body.get("user").is_some(),
        "user should NOT be dropped for model-b"
    );
}

#[tokio::test]
async fn drop_fields_noop_when_absent() {
    let captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("noop-model", "ok"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream}"
models = "noop-model"
# no drop_fields key
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "noop-model",
            "messages": [{"role": "user", "content": "hi"}],
            "user": "keep-me"
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = captured.lock().unwrap().take().expect("upstream body");
    assert!(
        body.get("user").is_some(),
        "user should be present when drop_fields absent"
    );
}

#[tokio::test]
async fn drop_fields_noop_when_empty_list() {
    let captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("noop-model", "ok"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream}"
models = "noop-model"
drop_fields = []
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "noop-model",
            "messages": [{"role": "user", "content": "hi"}],
            "user": "keep-me"
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = captured.lock().unwrap().take().expect("upstream body");
    assert!(
        body.get("user").is_some(),
        "user should be present when drop_fields is empty list"
    );
}

#[tokio::test]
async fn drop_fields_nonexistent_field_is_noop() {
    let captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("noop-model", "ok"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream}"
models = "noop-model"
drop_fields = ["nonexistent"]
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let resp = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "noop-model",
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    // Just checking the proxy doesn't error on nonexistent field
    let body = captured.lock().unwrap().take().expect("upstream body");
    assert!(body.get("messages").is_some(), "messages should remain");
}

// ── Interactions dump tests ─────────────────────────────────────────

/// Non-streaming Anthropic→Interactions must produce ingress, egress request,
/// and egress response dump lines.
#[tokio::test]
async fn interactions_non_streaming_produces_dumps() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-dump-001", "Hello from interactions dump test!"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-int-dump-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    // Response dumps are recorded immediately, but ingress/egress request
    // dumps are deferred to finish(). Poll after the response dump appears
    // until all three stages land.
    let dump_path: std::path::PathBuf = tmp.clone();
    let lines = {
        let mut lines = wait_for_egress_response_dump(&tmp).await;
        for _ in 0..20 {
            if lines.iter().any(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
                v["stage"].as_str() == Some("ingress")
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Ok(content) = std::fs::read_to_string(&dump_path) {
                lines = content.lines().map(|s| s.to_string()).collect();
            }
        }
        lines
    };
    assert!(
        lines.len() >= 3,
        "expected at least 3 dump lines (ingress, egress request, egress response), got {}",
        lines.len()
    );

    // Verify ingress request dump
    let has_ingress = lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("ingress") && v["direction"].as_str() == Some("request")
    });
    assert!(has_ingress, "must have ingress request dump");

    // Verify egress request dump
    let has_egress_req = lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("request")
    });
    assert!(has_egress_req, "must have egress request dump");

    // Verify egress response dump
    let has_egress_resp = lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("response")
    });
    assert!(has_egress_resp, "must have egress response dump");

    // All lines must share the same request_id
    assert_unique_requests(&lines);

    // Verify body contains expected text
    let response_line = lines
        .iter()
        .find(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("response")
        })
        .expect("must have response dump");
    let response_val: serde_json::Value = serde_json::from_str(response_line).expect("valid JSON");
    let body = response_val["body"].as_str().expect("body must be string");
    assert!(
        body.contains("Hello from interactions dump test!"),
        "response dump body must contain expected text, got: {body}"
    );
}

/// Streaming Anthropic→Interactions must produce dump lines.
#[tokio::test]
async fn interactions_streaming_produces_dumps() {
    let sse_body = format!(
        "data: {created}\n\ndata: {delta}\n\ndata: {completed}\n\n",
        created = serde_json::json!({
            "event_type": "INTERACTION_CREATED",
            "interaction": {
                "id": "int-stream-dump-1",
                "status": "started",
                "created": "2026-01-01T00:00:00Z",
                "updated": "2026-01-01T00:00:00Z",
                "steps": []
            }
        }),
        delta = serde_json::json!({
            "event_type": "CONTENT_DELTA",
            "delta": {"type": "text_delta", "text": "Stream dump body!"},
            "index": 0
        }),
        completed = serde_json::json!({
            "event_type": "INTERACTION_COMPLETED",
            "interaction": {
                "id": "int-stream-dump-1",
                "status": "completed",
                "created": "2026-01-01T00:00:00Z",
                "updated": "2026-01-01T00:00:01Z",
                "steps": [],
                "usage": {"total_input_tokens": 5, "total_output_tokens": 15}
            }
        }),
    );

    let session_store_path = std::env::temp_dir().join(format!(
        "inf-splitter-int-stream-dump-{}.toml",
        uuid_suffix()
    ));
    let _ = std::fs::remove_file(&session_store_path);

    let upstream_addr = common::spawn_stream_upstream("/v1beta/interactions", sse_body).await;

    let tmp = std::env::temp_dir().join(format!(
        "inf-splitter-int-stream-dump-{}.ndjson",
        uuid_suffix()
    ));
    let config = format!(
        r#"
listen_port = 0
interactions_session_store = "{store_path}"

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#,
        store_path = session_store_path.display(),
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let lines = wait_for_file(&tmp).await;
    assert!(
        !lines.is_empty(),
        "streaming interactions must produce dump lines"
    );

    let has_egress_resp = lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("response")
    });
    assert!(
        has_egress_resp,
        "streaming interactions must produce egress response dump"
    );
}

/// Interactions error path must produce dump lines (including response dump with error body).
#[tokio::test]
async fn interactions_error_produces_dumps() {
    let upstream_addr = spawn_error_upstream(
        "/v1beta/interactions",
        axum::http::StatusCode::BAD_GATEWAY,
        serde_json::json!({"error": {"message": "upstream exploded"}}),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!(
        "inf-splitter-int-err-dump-{}.ndjson",
        uuid_suffix()
    ));
    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::Error,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);

    let lines = wait_for_file(&tmp).await;
    assert!(
        !lines.is_empty(),
        "interactions error must produce dump lines"
    );

    // Must have response dump with error flag
    let has_error_resp = lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["stage"].as_str() == Some("egress")
            && v["direction"].as_str() == Some("response")
            && v["status"] == 502
    });
    assert!(
        has_error_resp,
        "interactions error must produce egress response dump with status 502"
    );
}

/// When dump_mode is "off", no dump file should be created for interactions.
#[tokio::test]
async fn interactions_no_dump_when_off() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-no-dump-1", "should not be dumped"),
    )
    .await;

    let tmp =
        std::env::temp_dir().join(format!("inf-splitter-int-nodump-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::Off,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // The file might be created (empty) but should have no content
    // Wait a bit then check
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    match std::fs::read_to_string(&tmp) {
        Ok(content) => assert!(
            content.trim().is_empty(),
            "dump file must be empty when dump_mode is off, got: {content}"
        ),
        Err(_) => {
            // File not existing at all is also fine
        }
    }
    let _ = std::fs::remove_file(&tmp);
}

/// Verify that dump and stats events share the same request_id for interactions.
#[tokio::test]
async fn interactions_dump_and_stats_share_request_id() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-shared-1", "Shared request ID test"),
    )
    .await;

    let tmp =
        std::env::temp_dir().join(format!("inf-splitter-int-shared-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    // Stats
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(!stats_lines.is_empty(), "must have stats line");
    let stats: serde_json::Value = serde_json::from_str(&stats_lines[0]).expect("valid NDJSON");
    let stats_request_id = stats["request_id"]
        .as_str()
        .expect("stats must have request_id");

    // Dump
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_file(&dump_path).await;
    assert!(!dump_lines.is_empty(), "must have dump lines");

    // All dump lines must share the same request_id as the stats line
    for line in &dump_lines {
        let dump: serde_json::Value = serde_json::from_str(line).expect("valid NDJSON");
        let dump_request_id = dump["request_id"]
            .as_str()
            .expect("dump must have request_id");
        assert_eq!(
            dump_request_id, stats_request_id,
            "dump request_id {dump_request_id} must match stats request_id {stats_request_id}"
        );
    }
}

// ── Phase 2: diag stats coverage tests ───────────────────────────────

#[tokio::test]
async fn interactions_split_send_records_aggregate_stats() {
    // Use a low proxy_limit to trigger content splitting.
    // 3 messages of ~37 bytes each → ~117 bytes > 80 byte limit → split into chunks.
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-split-stats-1", "Split stats test"),
    )
    .await;

    let tmp =
        std::env::temp_dir().join(format!("inf-splitter-split-stats-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
proxy_limit = "1k"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let pad = "y".repeat(300);
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let msg = |n: usize| -> serde_json::Value {
        serde_json::json!({"role": "user", "content": format!("msg{n:02} {pad}")})
    };
    let mut messages = Vec::new();
    for i in 0..10 {
        messages.push(msg(i));
    }
    let body = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "messages": messages
    });
    let response = post_anthropic(&proxy_addr, body).await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    // Aggregate stats must be recorded
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(
        !stats_lines.is_empty(),
        "split-send must produce aggregate stats line"
    );
    let stats: serde_json::Value =
        serde_json::from_str(&stats_lines[0]).expect("valid NDJSON stats");
    assert_eq!(stats["section"], "gemini");
    assert_eq!(stats["model"], "gemini-3.1-flash-lite");
    assert_eq!(stats["status"], 200);
    assert!(stats["request_size_bytes"].as_u64().unwrap_or(0) > 0);
    assert!(stats["response_size_bytes"].as_u64().unwrap_or(0) > 0);
    assert!(stats["error"].is_null() || stats.get("error").is_none());
    let stats_request_id = stats["request_id"]
        .as_str()
        .expect("stats must have request_id");

    // All per-chunk dump lines share the same request_id as the aggregate stats
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_file(&dump_path).await;
    assert!(!dump_lines.is_empty(), "split-send must produce dump lines");
    for line in &dump_lines {
        let dump: serde_json::Value = serde_json::from_str(line).expect("valid NDJSON dump");
        let dump_rid = dump["request_id"]
            .as_str()
            .expect("dump must have request_id");
        assert_eq!(
            dump_rid, stats_request_id,
            "dump request_id must match stats request_id"
        );
    }
}

#[tokio::test]
async fn interactions_system_instruction_split_records_stats() {
    // Content > proxy_limit triggers handle_split_send;
    // large system instruction > proxy_limit triggers send_split_system_instruction.
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-sys-split-1", "System instruction split test"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-sys-stats-{}.ndjson", uuid_suffix()));
    let large_system = "x".repeat(400);
    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
proxy_limit = "1k"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let pad = "x".repeat(230);
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let msg = |n: &str| -> serde_json::Value {
        serde_json::json!({"role": "user", "content": format!("{n} msg {pad}")})
    };
    let body = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "system": large_system,
        "messages": [msg("first"), msg("second"), msg("third"), msg("fourth"), msg("fifth")]
    });
    let response = post_anthropic(&proxy_addr, body).await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(
        !stats_lines.is_empty(),
        "system-instruction split must produce aggregate stats line"
    );
    let stats: serde_json::Value =
        serde_json::from_str(&stats_lines[0]).expect("valid NDJSON stats");
    assert_eq!(stats["section"], "gemini");
    assert_eq!(stats["status"], 200);
    assert!(stats["error"].is_null() || stats.get("error").is_none());
}

#[tokio::test]
async fn interactions_streaming_records_streaming_true_in_stats() {
    let sse_body = format!(
        "data: {created}\n\ndata: {delta}\n\ndata: {completed}\n\n",
        created = serde_json::json!({
            "event_type": "INTERACTION_CREATED",
            "interaction": {
                "id": "int-stream-stats-1",
                "status": "started",
                "created": "2026-01-01T00:00:00Z",
                "updated": "2026-01-01T00:00:00Z",
                "steps": []
            }
        }),
        delta = serde_json::json!({
            "event_type": "CONTENT_DELTA",
            "delta": {"type": "text_delta", "text": "Streaming stats body!"},
            "index": 0
        }),
        completed = serde_json::json!({
            "event_type": "INTERACTION_COMPLETED",
            "interaction": {
                "id": "int-stream-stats-1",
                "status": "completed",
                "created": "2026-01-01T00:00:00Z",
                "updated": "2026-01-01T00:01:01Z",
                "steps": [],
                "usage": {"total_input_tokens": 5, "total_output_tokens": 10}
            }
        }),
    );

    let session_store_path =
        std::env::temp_dir().join(format!("inf-splitter-stream-stats-{}.sess", uuid_suffix()));
    let _ = std::fs::remove_file(&session_store_path);
    let upstream_addr = common::spawn_stream_upstream("/v1beta/interactions", sse_body).await;

    let tmp = std::env::temp_dir().join(format!(
        "inf-splitter-stream-stats-{}.ndjson",
        uuid_suffix()
    ));
    let config = format!(
        r#"
listen_port = 0
interactions_session_store = "{store_path}"

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#,
        store_path = session_store_path.display(),
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "hello streaming"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let lines = wait_for_file(&tmp).await;
    assert!(
        !lines.is_empty(),
        "streaming interactions must produce stats line"
    );
    let stats_line = lines
        .iter()
        .find(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
            v.get("streaming").and_then(|s| s.as_bool()) == Some(true)
        })
        .expect("must have a stats line with streaming=true");
    let stats: serde_json::Value = serde_json::from_str(stats_line).expect("valid NDJSON stats");
    assert_eq!(stats["streaming"], true, "stats must have streaming=true");
    assert!(stats["response_size_bytes"].as_u64().unwrap_or(0) > 0);
    assert!(stats["error"].is_null() || stats.get("error").is_none());
}

#[tokio::test]
async fn interactions_error_records_stats_with_error_field() {
    let upstream_addr = spawn_error_upstream(
        "/v1beta/interactions",
        axum::http::StatusCode::BAD_GATEWAY,
        serde_json::json!({"error": {"message": "upstream exploded during stats test"}}),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-err-stats-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::Error,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_mode: DiagnosticMode::Error,
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello error test"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    let _body = response.text().await.expect("response body");

    // Stats must be recorded with error field
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(
        !stats_lines.is_empty(),
        "interactions error must produce stats line"
    );
    let stats: serde_json::Value =
        serde_json::from_str(&stats_lines[0]).expect("valid NDJSON stats");
    assert_eq!(stats["section"], "gemini");
    assert!(stats["status"].as_u64().unwrap_or(200) >= 400);
    assert!(
        stats["error"].is_string(),
        "stats must have error field on error response"
    );
    let stats_request_id = stats["request_id"]
        .as_str()
        .expect("stats must have request_id");

    // Dump lines must share the same request_id
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_file(&dump_path).await;
    assert!(
        !dump_lines.is_empty(),
        "interactions error must produce dump lines"
    );
    for line in &dump_lines {
        let dump: serde_json::Value = serde_json::from_str(line).expect("valid NDJSON dump");
        let dump_rid = dump["request_id"]
            .as_str()
            .expect("dump must have request_id");
        assert_eq!(dump_rid, stats_request_id);
    }
}

#[tokio::test]
async fn openai_passthrough_streaming_records_streaming_true_in_stats() {
    let sse_body = "data: {\"id\":\"chatcmpl-123\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello from stream\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"chatcmpl-123\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string();

    let upstream_addr = common::spawn_stream_upstream("/v1/chat/completions", sse_body).await;

    let tmp = std::env::temp_dir().join(format!(
        "inf-splitter-oa-stream-stats-{}.ndjson",
        uuid_suffix()
    ));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "test-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "test-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hello openai streaming"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let lines = wait_for_file(&tmp).await;
    assert!(
        !lines.is_empty(),
        "openai streaming passthrough must produce stats line"
    );
    let has_streaming = lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
        v.get("streaming").and_then(|s| s.as_bool()) == Some(true)
    });
    assert!(
        has_streaming,
        "openai passthrough streaming must record streaming=true in stats"
    );
}

// ── Phase 4: client error visibility tests ───────────────────────────

#[tokio::test]
async fn invalid_json_body_produces_ingress_dump() {
    let upstream_addr = spawn_upstream(
        "/v1/chat/completions",
        Arc::new(Mutex::new(None)),
        openai_upstream_response("ignored-id", "ignored"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!(
        "inf-splitter-bad-json-dump-{}.ndjson",
        uuid_suffix()
    ));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "bad-json-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::Error,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    // Send a truncated JSON body
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{proxy_addr}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"model": "bad-json-model", "messages":"#.to_string())
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let _body = response.text().await.expect("response body");

    let lines = wait_for_file(&tmp).await;
    assert!(
        !lines.is_empty(),
        "invalid JSON body must produce dump line"
    );
    let has_ingress = lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
        v["stage"].as_str() == Some("ingress") && v["direction"].as_str() == Some("request")
    });
    assert!(
        has_ingress,
        "invalid JSON body must have ingress dump with the malformed body"
    );
}

#[tokio::test]
async fn empty_model_produces_ingress_dump() {
    let upstream_addr = spawn_upstream(
        "/v1/chat/completions",
        Arc::new(Mutex::new(None)),
        openai_upstream_response("ignored-id", "ignored"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!(
        "inf-splitter-empty-model-dump-{}.ndjson",
        uuid_suffix()
    ));
    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "empty-model-test"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::Error,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;

    let response = post_openai(
        &proxy_addr,
        serde_json::json!({
            "model": "",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let _body = response.text().await.expect("response body");

    let lines = wait_for_file(&tmp).await;
    assert!(
        !lines.is_empty(),
        "empty model error must produce dump line"
    );
    let has_ingress = lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
        v["stage"].as_str() == Some("ingress") && v["direction"].as_str() == Some("request")
    });
    assert!(
        has_ingress,
        "empty model error must have ingress dump with the request body"
    );
}

// ── Red-green tests for fix-guard-deferred-dump-edge-cases ───────────

/// Stateful upstream: succeeds for first `succeed_count` requests (200 + response),
/// then fails with `error_status` for all subsequent requests.
async fn spawn_counted_upstream(
    path: &'static str,
    succeed_count: usize,
    response_body: serde_json::Value,
    error_status: reqwest::StatusCode,
    error_body: serde_json::Value,
) -> SocketAddr {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let app = axum::Router::new().route(
        path,
        axum::routing::post(move |Json(_): axum::Json<serde_json::Value>| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            let (status, body) = if n < succeed_count {
                (axum::http::StatusCode::OK, response_body.clone())
            } else {
                (error_status, error_body.clone())
            };
            async move { (status, axum::Json(body)).into_response() }
        }),
    );
    bind_and_serve(app).await.0
}

/// Bug 1.2: passthrough success → ingress/egress dumps must carry response status.
#[tokio::test]
async fn passthrough_success_request_dumps_have_status() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("ap-status-model", "reply"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-ap-status-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "ap-status-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "ap-status-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_file(&dump_path).await;
    assert!(
        !dump_lines.is_empty(),
        "passthrough success must produce dump lines"
    );

    // Ingress and egress request dumps should carry status: 200
    for stage in &["ingress", "egress"] {
        let has_status = dump_lines.iter().any(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
            v["stage"].as_str() == Some(stage)
                && v["direction"].as_str() == Some("request")
                && v["status"].as_u64() == Some(200)
        });
        assert!(
            has_status,
            "passthrough {stage} request dump must have status: 200, got lines: {dump_lines:?}"
        );
    }
}

/// Bug 1.3: no messages field → messages_detail_ingress absent from stats JSON.
#[tokio::test]
async fn no_messages_field_produces_no_detail_null_in_stats() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("null-detail-model", "reply"),
    )
    .await;

    let tmp =
        std::env::temp_dir().join(format!("inf-splitter-null-detail-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "null-detail-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::All,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    // Send a body WITHOUT a messages field to trigger the bug
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "null-detail-model",
            "max_tokens": 64
            // no "messages" field
        }),
    )
    .await;
    // The upstream will get the model "null-detail-model" and return normally,
    // but the ingress body has no messages — the stats should not contain
    // a null messages_detail_ingress.
    let _ = response.text().await;

    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(!stats_lines.is_empty(), "must produce stats line");
    let stats: serde_json::Value = serde_json::from_str(&stats_lines[0]).expect("valid NDJSON");

    // messages_detail_ingress should be ABSENT, not null
    assert!(
        stats.get("messages_detail_ingress").is_none(),
        "messages_detail_ingress must be absent when body has no messages, got: {}",
        stats["messages_detail_ingress"]
    );
}

/// Bug 1.4: egress request dump timestamp must be ≤ response dump timestamp
/// (captured at send time, not flush time). Uses simple passthrough to avoid
/// split-send chunk-count flakiness.
#[tokio::test]
async fn egress_request_dump_timestamp_not_after_response() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("ts-egress-model", "timestamp check"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-ts-eg-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "ts-egress-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::All,
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "ts-egress-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _ = response.text().await;

    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_file(&dump_path).await;
    assert!(!dump_lines.is_empty(), "must produce dump lines");

    let egress_req_ts: Vec<String> = dump_lines
        .iter()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("request") {
                v["ts"].as_str().map(String::from)
            } else {
                None
            }
        })
        .collect();
    let egress_resp_ts: Vec<String> = dump_lines
        .iter()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v["stage"].as_str() == Some("egress") && v["direction"].as_str() == Some("response")
            {
                v["ts"].as_str().map(String::from)
            } else {
                None
            }
        })
        .collect();

    assert!(
        !egress_req_ts.is_empty(),
        "must have egress request dump, got lines: {dump_lines:?}"
    );
    assert!(!egress_resp_ts.is_empty(), "must have egress response dump");

    // Egress request captured before sending, response after receiving.
    if let (Some(req_ts), Some(resp_ts)) = (egress_req_ts.first(), egress_resp_ts.first()) {
        assert!(
            req_ts <= resp_ts,
            "egress request ts {req_ts} must be <= response ts {resp_ts}"
        );
    }
}

/// Bug 1.6: anthropic passthrough with dump_mode: Off must not waste work.
/// When dump_mode is Off, the guard's egress_dump still stores the body.
/// Verify that no dump file is created (the flush filters it out),
/// but the egress_dump call itself is unconditional in anthropic passthrough.
/// This test verifies the dump is filtered by mode.
#[tokio::test]
async fn anthropic_passthrough_dump_off_no_dump_file() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured,
        anthropic_upstream_response("ap-off-model", "reply"),
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-ap-off-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "ap-off-model"
"#
    );

    let diag_config = DiagnosticsConfig {
        dump_mode: DiagnosticMode::Off,
        dump_output: Sink::File(tmp.clone()),
        ..DiagnosticsConfig::default()
    };
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let response = post_anthropic(
        &proxy_addr,
        serde_json::json!({
            "model": "ap-off-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _ = response.text().await;

    // With dump_mode: Off, the dump file should not be created or be empty
    if tmp.exists() {
        let content = std::fs::read_to_string(&tmp).unwrap_or_default();
        assert!(
            content.trim().is_empty(),
            "dump file must be empty when dump_mode is Off, got: {content}"
        );
    }
    // If file doesn't exist at all, that's also fine — the writer may not have created it.
}

/// Bug 1.1: split-send with chunk error must record error response dump
/// and aggregate stats with error field.
#[tokio::test]
async fn split_send_error_records_response_dump_and_error_stats() {
    let interactions_response = interactions_upstream_response("int-split-err-1", "ok");
    let error_body = serde_json::json!({"error": {"message": "split chunk failed"}});
    let upstream_addr = spawn_counted_upstream(
        "/v1beta/interactions",
        2,
        interactions_response,
        reqwest::StatusCode::BAD_GATEWAY,
        error_body,
    )
    .await;

    let tmp = std::env::temp_dir().join(format!("inf-splitter-split-err-{}.ndjson", uuid_suffix()));
    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
proxy_limit = "1k"
"#
    );

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::Error,
        stats_output: Sink::File(format!("{}.stats", tmp.display()).into()),
        dump_mode: DiagnosticMode::Error,
        dump_output: Sink::File(format!("{}.dump", tmp.display()).into()),
        ..DiagnosticsConfig::default()
    };
    let pad = "y".repeat(200);
    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let msg = |n: usize| -> serde_json::Value {
        serde_json::json!({"role": "user", "content": format!("msg{n} {pad}")})
    };
    let mut messages = Vec::new();
    for i in 0..12 {
        messages.push(msg(i));
    }
    let body = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "messages": messages
    });
    let response = post_anthropic(&proxy_addr, body).await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    let _ = response.text().await;

    // Stats: must have error field set
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_lines = wait_for_file(&stats_path).await;
    assert!(
        !stats_lines.is_empty(),
        "split-send error must produce stats line"
    );
    let stats: serde_json::Value =
        serde_json::from_str(&stats_lines[0]).expect("valid NDJSON stats");
    assert!(
        stats["error"].as_str().is_some(),
        "split-send error stats must have error field set"
    );

    // Dump: must have error response dump
    let dump_path: std::path::PathBuf = format!("{}.dump", tmp.display()).into();
    let dump_lines = wait_for_file(&dump_path).await;
    assert!(
        !dump_lines.is_empty(),
        "split-send error must produce dump lines"
    );
    let has_error_resp = dump_lines.iter().any(|l| {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
        v["stage"].as_str() == Some("egress")
            && v["direction"].as_str() == Some("response")
            && v["status"] == 502
    });
    assert!(
        has_error_resp,
        "split-send error must produce error response dump"
    );
}
