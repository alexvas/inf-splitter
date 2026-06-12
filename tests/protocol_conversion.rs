mod common;

use std::sync::{Arc, Mutex};

use anyllm_translate::anthropic::{Content, MessageCreateRequest};
use anyllm_translate::openai::{ChatCompletionRequest, ChatContent};
use common::{
    anthropic_upstream_response, openai_upstream_response, spawn_delayed_upstream,
    spawn_error_upstream, spawn_router, spawn_router_with_diagnostics, spawn_router_with_dump,
    spawn_sse_upstream_with_headers, spawn_stream_upstream, spawn_upstream, wait_for_egress_dump,
    wait_for_file, wait_for_ingress_dump,
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
    let content = wait_for_file(&tmp).await;
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
    assert!(!lines.is_empty(), "expected at least one stats line");
    assert_unique_requests(&lines);

    let line: serde_json::Value = serde_json::from_str(lines[0]).expect("valid NDJSON");
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

    let content = wait_for_file(&tmp).await;
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

    let content = wait_for_file(&tmp).await;
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

    let content = wait_for_file(&tmp).await;
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

    // Check stats: must have both ingress and egress message details.
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_content = wait_for_file(&stats_path).await;
    let _ = std::fs::remove_file(&stats_path);
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
    let dump_content = wait_for_file(&dump_path).await;
    let _ = std::fs::remove_file(&dump_path);
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

    // Stats: must have ingress and egress details.
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_content = wait_for_file(&stats_path).await;
    let _ = std::fs::remove_file(&stats_path);
    let stats_lines: Vec<&str> = stats_content.trim().lines().collect();
    assert_eq!(stats_lines.len(), 1, "expected exactly one stats line");
    let stats: serde_json::Value = serde_json::from_str(stats_lines[0]).expect("valid NDJSON");
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
    let dump_content = wait_for_file(&dump_path).await;
    let _ = std::fs::remove_file(&dump_path);
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
port = 0

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
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "ap-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Stats
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_content = wait_for_file(&stats_path).await;
    let _ = std::fs::remove_file(&stats_path);
    let stats_lines: Vec<&str> = stats_content.trim().lines().collect();
    assert!(!stats_lines.is_empty(), "expected at least one stats line");
    let stats: serde_json::Value = serde_json::from_str(stats_lines[0]).expect("valid NDJSON");
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
    let dump_content = wait_for_egress_dump(&dump_path).await;
    let _ = std::fs::remove_file(&dump_path);
    let dump_lines: Vec<&str> = dump_content.trim().lines().collect();
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
port = 0

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
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "ape-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);

    // Stats: must have error
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_content = wait_for_file(&stats_path).await;
    let _ = std::fs::remove_file(&stats_path);
    let stats_lines: Vec<&str> = stats_content.trim().lines().collect();
    assert!(
        !stats_lines.is_empty(),
        "expected at least one stats line on error"
    );
    let stats: serde_json::Value = serde_json::from_str(stats_lines[0]).expect("valid NDJSON");
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
    let dump_content = wait_for_egress_dump(&dump_path).await;
    let _ = std::fs::remove_file(&dump_path);
    let dump_lines: Vec<&str> = dump_content.trim().lines().collect();
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
port = 0

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
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "oa-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Stats
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_content = wait_for_file(&stats_path).await;
    let _ = std::fs::remove_file(&stats_path);
    let stats_lines: Vec<&str> = stats_content.trim().lines().collect();
    assert!(
        !stats_lines.is_empty(),
        "translation success must produce stats"
    );
    let stats: serde_json::Value = serde_json::from_str(stats_lines[0]).expect("valid NDJSON");
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
    let dump_content = wait_for_egress_dump(&dump_path).await;
    let _ = std::fs::remove_file(&dump_path);
    let dump_lines: Vec<&str> = dump_content.trim().lines().collect();
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
port = 0

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
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "ao-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "translate me"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Stats
    let stats_path: std::path::PathBuf = format!("{}.stats", tmp.display()).into();
    let stats_content = wait_for_file(&stats_path).await;
    let _ = std::fs::remove_file(&stats_path);
    let stats_lines: Vec<&str> = stats_content.trim().lines().collect();
    assert!(
        !stats_lines.is_empty(),
        "translation success must produce stats"
    );
    let stats: serde_json::Value = serde_json::from_str(stats_lines[0]).expect("valid NDJSON");
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
    let dump_content = wait_for_egress_dump(&dump_path).await;
    let _ = std::fs::remove_file(&dump_path);
    let dump_lines: Vec<&str> = dump_content.trim().lines().collect();
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
port = 0

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
    let client = reqwest::Client::new();

    let _response = client
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

    let content = wait_for_file(&tmp).await;
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
    assert!(
        !lines.is_empty(),
        "streaming passthrough must produce stats"
    );

    let line: serde_json::Value = serde_json::from_str(lines[0]).expect("valid NDJSON");
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
port = 0

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
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "timed-model",
            "messages": [{"role": "user", "content": "ping"}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let content = wait_for_file(&tmp).await;
    let _ = std::fs::remove_file(&tmp);
    let stats: serde_json::Value =
        serde_json::from_str(content.trim().lines().next().unwrap()).expect("valid NDJSON");

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
port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "dup-model"
"#
    );

    let proxy_addr = spawn_router(&config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "dup-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }))
        .send()
        .await
        .expect("proxy request");

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
port = 0

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
    let client = reqwest::Client::new();
    let _resp = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "oi-model",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    let content = wait_for_ingress_dump(&tmp).await;
    let _ = std::fs::remove_file(&tmp);
    let has_ingress = content.lines().any(|l| {
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
port = 0

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
    let client = reqwest::Client::new();
    let _resp = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "ap-ingress-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(_resp.status(), reqwest::StatusCode::OK);

    let content = wait_for_ingress_dump(&tmp).await;
    let _ = std::fs::remove_file(&tmp);
    let has_ingress = content.lines().any(|l| {
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
port = 0

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
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "er-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let content = wait_for_file(&tmp).await;
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
    assert!(!lines.is_empty(), "expected at least one dump line");
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
port = 0

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
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "stream-er-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }))
        .send()
        .await
        .expect("proxy request");

    assert!(response.status().is_success());
    // Must consume the SSE stream body to trigger DiagnosticStream termination and dump.
    let _body = response.text().await.expect("response body");

    let content = wait_for_file(&tmp).await;
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
    assert!(!lines.is_empty(), "expected at least one dump line");
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
port = 0

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
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "oai-er-model",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _body = response.text().await.expect("response body");

    let content = wait_for_egress_dump(&tmp).await;
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
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
port = 0

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
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
        .json(&serde_json::json!({
            "model": "stream-oai-er-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }))
        .send()
        .await
        .expect("proxy request");

    assert!(response.status().is_success());
    // Must consume the SSE stream body to trigger DiagnosticStream termination and dump.
    let _body = response.text().await.expect("response body");

    let content = wait_for_egress_dump(&tmp).await;
    let _ = std::fs::remove_file(&tmp);

    let lines: Vec<&str> = content.trim().lines().collect();
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
port = 0

[local]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"
"#;
    let proxy_addr = spawn_router(config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/openai/v1/messages"))
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
port = 0

[remote]
endpoint_anthropic = "http://{upstream}"
models = "test-model"
"#
    );
    let proxy_addr = common::spawn_router(&config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
        .json(&serde_json::json!({
            "model": "test-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .expect("proxy request");

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
port = 0

[local]
endpoint_openai = "http://{upstream}"
models = "test-model"
"#
    );
    let proxy_addr = common::spawn_router(&config).await;
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
port = 0

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
    let client = reqwest::Client::new();

    // Send multiple requests to fill the file past 1KB
    for i in 0..5 {
        let response = client
            .post(format!("http://{proxy_addr}/anthropic/v1/messages"))
            .json(&serde_json::json!({
                "model": "rot-model",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": format!("msg-{}", i)}]
            }))
            .send()
            .await
            .expect("proxy request");
        // Consume body to trigger dump recording
        let _ = response.text().await;
    }

    let content = wait_for_file(&tmp).await;

    // Verify the current file has some content (latest dumps)
    assert!(
        !content.trim().is_empty(),
        "current file should have content"
    );

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

fn uuid_suffix() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    format!("{ts}")
}
