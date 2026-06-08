mod common;

use std::sync::{Arc, Mutex};

use anyllm_translate::anthropic::{Content, MessageCreateRequest};
use anyllm_translate::openai::{ChatCompletionRequest, ChatContent};
use common::{
    anthropic_upstream_response, openai_upstream_response, spawn_error_upstream, spawn_router,
    spawn_router_with_diagnostics, spawn_router_with_dump, spawn_stream_upstream, spawn_upstream,
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
port = 0

[local]
endpoint_openai = "http://{openai_addr}"
models = "local-openai-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let client = reqwest::Client::new();

    let anthropic_request = serde_json::json!({
        "model": "local-openai-model",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": CLIENT_PROMPT}]
    });

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&anthropic_request)
        .send()
        .await
        .expect("proxy request");

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
port = 0

[remote]
endpoint_anthropic = "http://{anthropic_addr}"
models = "remote-anthropic-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let client = reqwest::Client::new();

    let openai_request = serde_json::json!({
        "model": "remote-anthropic-model",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": CLIENT_PROMPT}]
    });

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&openai_request)
        .send()
        .await
        .expect("proxy request");

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
port = 0

[local]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"
"#;
    let proxy_addr = spawn_router(config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "unknown-model",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn empty_model_returns_400() {
    let config = r#"
port = 0

[local]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"
"#;
    let proxy_addr = spawn_router(config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn invalid_json_body_returns_400() {
    let config = r#"
port = 0

[local]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"
"#;
    let proxy_addr = spawn_router(config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
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
port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "test-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

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
port = 0

[local]
endpoint_openai = "http://{openai_addr}"
models = "passthrough-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "passthrough-model",
            "messages": [{"role": "user", "content": "test"}]
        }))
        .send()
        .await
        .expect("proxy request");

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
port = 0

[remote]
endpoint_anthropic = "http://{anthropic_addr}"
models = "passthrough-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "passthrough-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "test"}]
        }))
        .send()
        .await
        .expect("proxy request");

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
port = 0

[remote]
endpoint_anthropic = "http://{anthropic_addr}"
models = "stream-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "stream-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }))
        .send()
        .await
        .expect("proxy request");

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
port = 0

[local]
endpoint_openai = "http://{openai_addr}"
models = "stream-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "stream-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }))
        .send()
        .await
        .expect("proxy request");

    assert!(response.status().is_success());
    let body = response.text().await.expect("response body");
    assert!(
        body.contains("content_block_delta") || body.contains("message_stop"),
        "expected Anthropic SSE events, got: {body}"
    );
}

/// With DUMP_ON_ERROR=1 the passthrough error path must still relay the
/// correct status and body while writing diagnostic JSON to stderr.
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
port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "dump-model"
"#
    );

    let proxy_addr = spawn_router_with_dump(&config).await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "dump-model",
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "world"}
            ]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"]["message"], "upstream exploded");
}

/// With DUMP_ON_ERROR=1 the conversion error path must still relay the
/// correct status and body (and messages_detail is populated from the
/// typed request).
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
port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "dump-conv-model"
"#
    );

    let proxy_addr = spawn_router_with_dump(&config).await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "dump-conv-model",
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "user", "content": [{"type": "text", "text": "describe this"}]}
            ]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(body["error"]["message"], "too busy");
}

/// With DUMP_ON_ERROR=1 and a stream request that fails at the HTTP level
/// (non-2xx from upstream), the error relay must still work.
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
port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "dump-stream-model"
"#
    );

    let proxy_addr = spawn_router_with_dump(&config).await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "dump-stream-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }))
        .send()
        .await
        .expect("proxy request");

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
port = 0

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

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "test-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Give the background writer a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let content = std::fs::read_to_string(&tmp).expect("read stats file");
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
    assert!(!lines.is_empty(), "expected at least one stats line");
    assert_unique_requests(&lines);

    let line: serde_json::Value = serde_json::from_str(lines[0]).expect("valid NDJSON");
    assert_eq!(line["direction"], "openai->openai");
    assert_eq!(line["model"], "test-model");
    assert_eq!(line["status"], 200);
    assert!(line["duration_ms"].as_u64().is_some());
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
port = 0

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

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "test-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let content = std::fs::read_to_string(&tmp).expect("read dump file");
    let _ = std::fs::remove_file(&tmp);

    assert!(!content.trim().is_empty(), "dump file should not be empty");
    let lines: Vec<&str> = content.trim().lines().collect();
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
port = 0

[ok]
endpoint_openai = "http://{success_upstream}"
models = "ok-model"

[fail]
endpoint_openai = "http://{error_upstream}"
models = "fail-model"
"#
    );

    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let client = reqwest::Client::new();

    // Successful request — should NOT write stats.
    let ok_resp = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "ok-model",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("ok request");
    assert_eq!(ok_resp.status(), reqwest::StatusCode::OK);

    // Error request — should write stats.
    let err_resp = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "fail-model",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("fail request");
    assert_eq!(err_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let content = std::fs::read_to_string(&tmp).expect("read stats file");
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "only the error request should produce a stats line, got {lines:?}"
    );
    assert_unique_requests(&lines);
    let line: serde_json::Value = serde_json::from_str(lines[0]).expect("valid NDJSON");
    assert_eq!(line["model"], "fail-model");
    assert!(line["error"].is_string());
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
port = 0

[ok]
endpoint_openai = "http://{success_upstream}"
models = "ok-model"

[fail]
endpoint_openai = "http://{error_upstream}"
models = "fail-model"
"#
    );

    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let client = reqwest::Client::new();

    // Successful request — should NOT write dump.
    let ok_resp = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "ok-model",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("ok request");
    assert_eq!(ok_resp.status(), reqwest::StatusCode::OK);

    // Error request — should write dump.
    let err_resp = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "fail-model",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("fail request");
    assert_eq!(err_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let content = std::fs::read_to_string(&tmp).expect("read dump file");
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
    assert!(
        !lines.is_empty(),
        "error request should produce at least one dump line"
    );
    assert_unique_requests(&lines);
    for line_str in lines {
        let line: serde_json::Value = serde_json::from_str(line_str).expect("valid NDJSON");
        assert_eq!(line["model"], "fail-model");
    }
}

/// Assert that every NDJSON line has a unique `(request_id, stage, direction)` tuple.
fn assert_unique_requests(lines: &[&str]) {
    let mut seen = std::collections::HashSet::new();
    for (i, line_str) in lines.iter().enumerate() {
        if line_str.trim().is_empty() {
            continue;
        }
        let line: serde_json::Value = serde_json::from_str(line_str).expect("valid NDJSON");
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
port = 0

[remote]
endpoint_openai = "http://{error_upstream}"
models = "trans-model"
"#
    );

    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "trans-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}]
        }))
        .send()
        .await
        .expect("proxy request");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Check stats: must have both ingress and egress message details.
    let stats_content =
        std::fs::read_to_string(format!("{}.stats", tmp.display())).expect("read stats");
    let _ = std::fs::remove_file(format!("{}.stats", tmp.display()));
    let stats_lines: Vec<&str> = stats_content.trim().lines().collect();
    assert_eq!(stats_lines.len(), 1, "expected exactly one stats line");
    let stats: serde_json::Value = serde_json::from_str(stats_lines[0]).expect("valid NDJSON");
    assert_eq!(stats["direction"], "anthropic->openai");
    assert!(
        stats["messages_detail_ingress"].is_array(),
        "translation stats must have messages_detail_ingress"
    );
    assert!(
        stats["messages_detail_egress"].is_array(),
        "translation stats must have messages_detail_egress"
    );

    // Check dump: must have both ingress and egress stage entries.
    let dump_content =
        std::fs::read_to_string(format!("{}.dump", tmp.display())).expect("read dump");
    let _ = std::fs::remove_file(format!("{}.dump", tmp.display()));
    let dump_lines: Vec<&str> = dump_content.trim().lines().collect();
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
port = 0

[remote]
endpoint_anthropic = "http://{error_upstream}"
models = "trans-model"
"#
    );

    let proxy_addr = spawn_router_with_diagnostics(&config, diag_config).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "trans-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}]
        }))
        .send()
        .await
        .expect("proxy request");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Stats: must have ingress and egress details.
    let stats_content =
        std::fs::read_to_string(format!("{}.stats", tmp.display())).expect("read stats");
    let _ = std::fs::remove_file(format!("{}.stats", tmp.display()));
    let stats_lines: Vec<&str> = stats_content.trim().lines().collect();
    assert_eq!(stats_lines.len(), 1, "expected exactly one stats line");
    let stats: serde_json::Value = serde_json::from_str(stats_lines[0]).expect("valid NDJSON");
    assert_eq!(stats["direction"], "openai->anthropic");
    assert!(stats["messages_detail_ingress"].is_array());
    assert!(stats["messages_detail_egress"].is_array());

    // Dump: must have ingress stage.
    let dump_content =
        std::fs::read_to_string(format!("{}.dump", tmp.display())).expect("read dump");
    let _ = std::fs::remove_file(format!("{}.dump", tmp.display()));
    let dump_lines: Vec<&str> = dump_content.trim().lines().collect();
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

fn uuid_suffix() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    format!("{ts}")
}
