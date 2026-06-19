mod common;

use std::sync::{Arc, Mutex};

use anyllm_translate::anthropic::{Content, MessageCreateRequest, MessageResponse};
use anyllm_translate::openai::{ChatCompletionRequest, ChatCompletionResponse, ChatContent};
use common::{
    anthropic_upstream_response, interactions_upstream_response, openai_upstream_response,
    post_anthropic, post_openai, spawn_router, spawn_upstream,
};

const PROMPT: &str = "hello-from-typed-client";

fn make_openai_request(model: &str) -> ChatCompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": model,
        "max_tokens": 64,
        "messages": [{"role": "user", "content": PROMPT}]
    }))
    .unwrap()
}

fn make_anthropic_request(model: &str) -> MessageCreateRequest {
    serde_json::from_value(serde_json::json!({
        "model": model,
        "max_tokens": 64,
        "messages": [{"role": "user", "content": PROMPT}]
    }))
    .unwrap()
}

// ── Passthrough: typed client ↔ proxy ↔ upstream ──

#[tokio::test]
async fn openai_client_to_openai_upstream_passthrough_roundtrip() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("passthrough-oa", "openai-upstream-reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[local]
endpoint_openai = "http://{upstream_addr}"
models = "passthrough-oa"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let request = make_openai_request("passthrough-oa");
    let response = post_openai(
        &proxy_addr,
        serde_json::to_value(&request).expect("serialize ChatCompletionRequest"),
    )
    .await;

    let status = response.status();
    let body = response.text().await.expect("response body");
    assert!(status.is_success(), "proxy failed with {status}: {body}");

    // Round-trip: deserialize response with the typed client.
    let typed: ChatCompletionResponse =
        serde_json::from_str(&body).expect("deserialize ChatCompletionResponse");
    assert_eq!(typed.object, "chat.completion");
    assert_eq!(typed.model, "passthrough-oa");

    // Upstream received a valid ChatCompletionRequest.
    let upstream_body = captured
        .lock()
        .expect("lock captured")
        .clone()
        .expect("upstream must receive request");
    let _upstream_req: ChatCompletionRequest =
        serde_json::from_value(upstream_body).expect("upstream body must be ChatCompletionRequest");
}

#[tokio::test]
async fn anthropic_client_to_anthropic_upstream_passthrough_roundtrip() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured.clone(),
        anthropic_upstream_response("passthrough-an", "anthropic-upstream-reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "passthrough-an"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let request = make_anthropic_request("passthrough-an");
    let response = post_anthropic(
        &proxy_addr,
        serde_json::to_value(&request).expect("serialize MessageCreateRequest"),
    )
    .await;

    let status = response.status();
    let body = response.text().await.expect("response body");
    assert!(status.is_success(), "proxy failed with {status}: {body}");

    // Round-trip: deserialize response with the typed client.
    let typed: MessageResponse = serde_json::from_str(&body).expect("deserialize MessageResponse");
    assert_eq!(typed.response_type, "message");
    assert_eq!(typed.model, "passthrough-an");

    // Upstream received a valid MessageCreateRequest.
    let upstream_body = captured
        .lock()
        .expect("lock captured")
        .clone()
        .expect("upstream must receive request");
    let _upstream_req: MessageCreateRequest =
        serde_json::from_value(upstream_body).expect("upstream body must be MessageCreateRequest");
}

// ── Cross: typed client → proxy (translate) → upstream ──

#[tokio::test]
async fn openai_client_to_anthropic_upstream_cross_roundtrip() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/messages",
        captured.clone(),
        anthropic_upstream_response("cross-model", "cross-anthropic-reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_anthropic = "http://{upstream_addr}"
models = "cross-model"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let request = make_openai_request("cross-model");
    let response = post_openai(
        &proxy_addr,
        serde_json::to_value(&request).expect("serialize ChatCompletionRequest"),
    )
    .await;

    let status = response.status();
    let body = response.text().await.expect("response body");
    assert!(status.is_success(), "proxy failed with {status}: {body}");

    // Client receives OpenAI-shaped response.
    let typed: ChatCompletionResponse =
        serde_json::from_str(&body).expect("deserialize ChatCompletionResponse");
    assert_eq!(typed.object, "chat.completion");
    assert_eq!(typed.model, "cross-model");

    // Upstream received a valid MessageCreateRequest (after translation).
    let upstream_body = captured
        .lock()
        .expect("lock captured")
        .clone()
        .expect("upstream must receive request");
    let upstream_req: MessageCreateRequest =
        serde_json::from_value(upstream_body).expect("upstream body must be MessageCreateRequest");
    assert_eq!(upstream_req.model, "cross-model");
    assert!(upstream_req.max_tokens > 0);
    assert!(!upstream_req.messages.is_empty());
    match &upstream_req.messages[0].content {
        Content::Text(text) => assert_eq!(text, PROMPT),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_client_to_openai_upstream_cross_roundtrip() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1/chat/completions",
        captured.clone(),
        openai_upstream_response("cross-model-2", "cross-openai-reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[remote]
endpoint_openai = "http://{upstream_addr}"
models = "cross-model-2"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let request = make_anthropic_request("cross-model-2");
    let response = post_anthropic(
        &proxy_addr,
        serde_json::to_value(&request).expect("serialize MessageCreateRequest"),
    )
    .await;

    let status = response.status();
    let body = response.text().await.expect("response body");
    assert!(status.is_success(), "proxy failed with {status}: {body}");

    // Client receives Anthropic-shaped response.
    let typed: MessageResponse = serde_json::from_str(&body).expect("deserialize MessageResponse");
    assert_eq!(typed.response_type, "message");
    assert_eq!(typed.model, "cross-model-2");

    // Upstream received a valid ChatCompletionRequest (after translation).
    let upstream_body = captured
        .lock()
        .expect("lock captured")
        .clone()
        .expect("upstream must receive request");
    let upstream_req: ChatCompletionRequest =
        serde_json::from_value(upstream_body).expect("upstream body must be ChatCompletionRequest");
    assert_eq!(upstream_req.model, "cross-model-2");
    assert!(!upstream_req.messages.is_empty());
    match &upstream_req.messages[0].content {
        Some(ChatContent::Text(text)) => assert_eq!(text, PROMPT),
        other => panic!("expected text content, got {other:?}"),
    }
}

// ── Routing: multi-provider model dispatch ──

/// Two providers with different explicit models — each model must hit its own upstream.
#[tokio::test]
async fn multi_provider_routes_to_correct_upstream() {
    let cap_a = Arc::new(Mutex::new(None::<serde_json::Value>));
    let cap_b = Arc::new(Mutex::new(None::<serde_json::Value>));

    let upstream_a = spawn_upstream(
        "/v1/chat/completions",
        cap_a.clone(),
        openai_upstream_response("model-a", "reply-from-a"),
    )
    .await;
    let upstream_b = spawn_upstream(
        "/v1/chat/completions",
        cap_b.clone(),
        openai_upstream_response("model-b", "reply-from-b"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[provider-a]
endpoint_openai = "http://{upstream_a}"
models = "model-a"

[provider-b]
endpoint_openai = "http://{upstream_b}"
models = "model-b"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    // Request to model-a → must hit upstream_a.
    let resp_a = post_openai(
        &proxy_addr,
        serde_json::to_value(&make_openai_request("model-a")).unwrap(),
    )
    .await;
    assert!(resp_a.status().is_success());
    let body_a: ChatCompletionResponse =
        resp_a.json().await.expect("ChatCompletionResponse from a");
    assert_eq!(body_a.model, "model-a");

    // Request to model-b → must hit upstream_b.
    let resp_b = post_openai(
        &proxy_addr,
        serde_json::to_value(&make_openai_request("model-b")).unwrap(),
    )
    .await;
    assert!(resp_b.status().is_success());
    let body_b: ChatCompletionResponse =
        resp_b.json().await.expect("ChatCompletionResponse from b");
    assert_eq!(body_b.model, "model-b");

    // Verify isolation: each upstream saw exactly its own model.
    let seen_a = cap_a
        .lock()
        .expect("lock cap_a")
        .clone()
        .expect("upstream a used");
    assert_eq!(seen_a["model"], "model-a");
    let seen_b = cap_b
        .lock()
        .expect("lock cap_b")
        .clone()
        .expect("upstream b used");
    assert_eq!(seen_b["model"], "model-b");
}

/// `models = "default"` catches any model not explicitly listed elsewhere.
#[tokio::test]
async fn default_provider_catches_unmatched_model() {
    let cap_default = Arc::new(Mutex::new(None::<serde_json::Value>));

    let upstream_default = spawn_upstream(
        "/v1/messages",
        cap_default.clone(),
        anthropic_upstream_response("catch-all", "default-reply"),
    )
    .await;

    // Provider with explicit model = "known", default catches everything else.
    let config = format!(
        r#"
listen_port = 0

[explicit]
endpoint_openai = "http://127.0.0.1:1"
models = "known-model"

[fallback]
endpoint_anthropic = "http://{upstream_default}"
models = "default"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    // Send an unknown model — must hit the default provider.
    let request = make_anthropic_request("some-unknown-model");
    let response = post_anthropic(&proxy_addr, serde_json::to_value(&request).unwrap()).await;

    assert!(response.status().is_success());
    let typed: MessageResponse = response.json().await.expect("MessageResponse");
    assert_eq!(typed.model, "catch-all");

    let seen = cap_default
        .lock()
        .expect("lock cap_default")
        .clone()
        .expect("default upstream must receive request");
    assert_eq!(seen["model"], "some-unknown-model");
}

/// `models = ["a", "b"]` — both model names route to the same provider.
#[tokio::test]
async fn model_list_routes_all_entries_to_same_upstream() {
    let cap = Arc::new(Mutex::new(None::<serde_json::Value>));

    let upstream = spawn_upstream(
        "/v1/chat/completions",
        cap.clone(),
        openai_upstream_response("model-list-up", "list-reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[multi]
endpoint_openai = "http://{upstream}"
models = ["alpha", "beta", "gamma"]
"#
    );
    let proxy_addr = spawn_router(&config).await;

    for model in ["alpha", "beta", "gamma"] {
        // Reset capture for each model.
        *cap.lock().expect("lock cap") = None;

        let resp = post_openai(
            &proxy_addr,
            serde_json::to_value(&make_openai_request(model)).unwrap(),
        )
        .await;
        assert!(resp.status().is_success(), "model {model} must succeed");

        let seen = cap
            .lock()
            .expect("lock cap")
            .clone()
            .expect("upstream must receive request");
        assert_eq!(seen["model"], model, "upstream must see model={model}");
    }
}

/// When a model is not listed and there is no default provider, proxy returns 400.
#[tokio::test]
async fn unroutable_model_returns_400_e2e() {
    let config = r#"
listen_port = 0

[only]
endpoint_openai = "http://127.0.0.1:1"
models = "sole-model"
"#;
    let proxy_addr = spawn_router(&config).await;

    let request = make_anthropic_request("ghost-model");
    let response = post_anthropic(&proxy_addr, serde_json::to_value(&request).unwrap()).await;

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert!(
        body.to_string().contains("ghost-model"),
        "error must mention the unknown model, got: {body}"
    );
}

// ── Interactions API E2E ──

/// Helper: POST to `/v1/messages` with a request_id header.
async fn post_anthropic_with_session(
    proxy_addr: &std::net::SocketAddr,
    body: serde_json::Value,
    session_id: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/messages"))
        .header("x-request-id", session_id)
        .json(&body)
        .send()
        .await
        .expect("proxy request")
}

/// POST to `/v1/chat/completions` with a request_id header.
async fn post_openai_with_session(
    proxy_addr: &std::net::SocketAddr,
    body: serde_json::Value,
    session_id: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/chat/completions"))
        .header("x-request-id", session_id)
        .json(&body)
        .send()
        .await
        .expect("proxy request")
}

#[tokio::test]
async fn anthropic_ingress_to_interactions_roundtrip() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-001", "Hello from Gemini Interactions!"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let request = make_anthropic_request("gemini-3.1-flash-lite");
    let response = post_anthropic(&proxy_addr, serde_json::to_value(&request).unwrap()).await;

    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "got status {status}, body: {body_text}"
    );
    let body: serde_json::Value = serde_json::from_str(&body_text).expect("valid json");
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(
        body["content"][0]["text"],
        "Hello from Gemini Interactions!"
    );
    // Upstream must have received the interactions-format request
    let upstream_body = captured.lock().unwrap().clone().expect("captured body");
    assert!(upstream_body["input"].is_array(), "must have 'input' array");
}

#[tokio::test]
async fn openai_ingress_to_interactions_roundtrip() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-002", "OpenAI→Interactions reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let request = make_openai_request("gemini-3.1-flash-lite");
    let response = post_openai(&proxy_addr, serde_json::to_value(&request).unwrap()).await;

    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "got status {status}, body: {body_text}"
    );
    let body: serde_json::Value = serde_json::from_str(&body_text).expect("valid json");
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert!(body["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("OpenAI→Interactions reply"));
    // Verify the upstream received interactions-format input
    let upstream_body = captured.lock().unwrap().clone().expect("captured body");
    assert!(upstream_body["input"].is_array());
}

#[tokio::test]
async fn interactions_multi_turn_session_delta() {
    // First turn: 1 message
    let captured1 = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured1.clone(),
        interactions_upstream_response("int-100", "Turn 1 reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    // Turn 1: send 1 message
    let request1 = make_anthropic_request("gemini-3.1-flash-lite");
    let response1 = post_anthropic_with_session(
        &proxy_addr,
        serde_json::to_value(&request1).unwrap(),
        "sess-delta-1",
    )
    .await;
    assert_eq!(response1.status(), reqwest::StatusCode::OK);
    let body1: serde_json::Value = response1.json().await.unwrap();
    assert_eq!(body1["id"], "int-100");

    // Verify turn 1 sent exactly 1 message
    let upstream1 = captured1.lock().unwrap().clone().expect("turn 1 body");
    let input1 = upstream1["input"].as_array().unwrap();
    assert_eq!(input1.len(), 1, "turn 1 should send 1 message");

    // Turn 2: send 2 messages (1 old + 1 new) — same session, same proxy instance
    // After turn 1, message_count=1. Turn 2 sends 2 messages total → delta skips 1
    let request2 = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "user", "content": "second message"}
        ]
    });
    let response2 = post_anthropic_with_session(&proxy_addr, request2, "sess-delta-1").await;
    assert_eq!(response2.status(), reqwest::StatusCode::OK);

    let upstream2 = captured1.lock().unwrap().clone().expect("turn 2 body");
    let input2 = upstream2["input"].as_array().unwrap();
    // Delta should skip the first message: only "second message" is new
    assert_eq!(
        input2.len(),
        1,
        "turn 2 delta should send only 1 new message"
    );
}

#[tokio::test]
async fn interactions_error_translation_e2e() {
    // Error translation requires a mock upstream that returns non-2xx status codes.
    // The standard spawn_upstream helper returns 200 OK unconditionally.
    // Full error translation testing is covered by unit tests in config.rs.
    // This test verifies the interactions error path is exercised.
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        serde_json::json!({"error": {"message": "quota exceeded", "code": 429}}),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let request = make_anthropic_request("gemini-3.1-flash-lite");
    let response = post_anthropic(&proxy_addr, serde_json::to_value(&request).unwrap()).await;

    // A non-Interaction response body that's still 200 OK will cause an upstream parse error
    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn interactions_token_limits_injected() {
    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-200", "Limited reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
max_tokens = 42
"#
    );
    let proxy_addr = spawn_router(&config).await;

    let request = make_anthropic_request("gemini-3.1-flash-lite");
    let response = post_anthropic(&proxy_addr, serde_json::to_value(&request).unwrap()).await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    // Verify max_tokens was injected into the request
    let upstream_body = captured.lock().unwrap().clone().expect("captured body");
    let gen_config = &upstream_body["generation_config"];
    assert_eq!(
        gen_config["max_output_tokens"], 42,
        "max_tokens should be injected as generation_config.max_output_tokens"
    );
}

// ── Session persistence ──

#[tokio::test]
async fn interactions_session_persistence_survives_restart() {
    let session_store_path = std::env::temp_dir().join(format!(
        "inf-splitter-test-sessions-{}.toml",
        std::process::id()
    ));
    // Clean up any leftover from previous failed test
    let _ = std::fs::remove_file(&session_store_path);

    // ── First proxy instance: establish a session ──
    let captured1 = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured1.clone(),
        interactions_upstream_response("int-persist-1", "Persisted reply"),
    )
    .await;

    let config1 = format!(
        r#"
listen_port = 0
interactions_session_store = "{store_path}"

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#,
        store_path = session_store_path.display(),
        upstream_addr = upstream_addr,
    );
    let proxy_addr1 = spawn_router(&config1).await;

    // Send first request — establishes session with 1 message delivered
    let request1 = make_anthropic_request("gemini-3.1-flash-lite");
    let response1 = post_anthropic_with_session(
        &proxy_addr1,
        serde_json::to_value(&request1).unwrap(),
        "sess-persist",
    )
    .await;
    assert_eq!(response1.status(), reqwest::StatusCode::OK);

    // Drop the first proxy
    drop(proxy_addr1);
    // Give it a moment to flush
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Verify the session file exists
    assert!(
        session_store_path.exists(),
        "session store should be persisted to disk"
    );

    // ── Second proxy instance: recover session and compute delta ──
    let captured2 = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr2 = spawn_upstream(
        "/v1beta/interactions",
        captured2.clone(),
        interactions_upstream_response("int-persist-2", "Second reply"),
    )
    .await;

    let config2 = format!(
        r#"
listen_port = 0
interactions_session_store = "{store_path}"

[gemini]
endpoint_interactions = "http://{upstream_addr2}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
"#,
        store_path = session_store_path.display(),
        upstream_addr2 = upstream_addr2,
    );
    let proxy_addr2 = spawn_router(&config2).await;

    // Send second request with 2 messages (1 old + 1 new) — delta should skip the first
    let request2 = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "user", "content": "new message after restart"}
        ]
    });
    let response2 = post_anthropic_with_session(&proxy_addr2, request2, "sess-persist").await;
    assert_eq!(response2.status(), reqwest::StatusCode::OK);

    // Delta should have skipped the already-delivered message
    let upstream2 = captured2.lock().unwrap().clone().expect("turn 2 body");
    let input2 = upstream2["input"].as_array().unwrap();
    assert_eq!(
        input2.len(),
        1,
        "after restart, delta should send only 1 new message"
    );

    // Cleanup
    let _ = std::fs::remove_file(&session_store_path);
}

// ── Control messages ──

const CTRL_CLEAN_ALL: &str = "***!___!--- clear all sessions for test ---!___!***";
const CTRL_EXTEND: &str = "***!___!--- extend session test to <unix_utc> ---!___!***";

#[tokio::test]
async fn control_message_clean_all_sessions() {
    let session_path = std::env::temp_dir().join(format!(
        "inf-splitter-ctrl-clean-{}.toml",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&session_path);

    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-ctrl-1", "Reply before clean"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0
interactions_session_store = "{store_path}"

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
control_clean_all = "{CTRL_CLEAN_ALL}"
"#,
        store_path = session_path.display(),
        CTRL_CLEAN_ALL = CTRL_CLEAN_ALL,
    );
    let proxy_addr = spawn_router(&config).await;

    // First, create a session with a normal message
    let request1 = make_anthropic_request("gemini-3.1-flash-lite");
    let response1 = post_anthropic_with_session(
        &proxy_addr,
        serde_json::to_value(&request1).unwrap(),
        "sess-ctrl",
    )
    .await;
    assert_eq!(response1.status(), reqwest::StatusCode::OK);

    // Send the clean-all control message
    let clean_request = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": CTRL_CLEAN_ALL}
        ]
    });
    let response2 = post_anthropic_with_session(&proxy_addr, clean_request, "sess-ctrl").await;
    assert_eq!(response2.status(), reqwest::StatusCode::OK);
    let body2: serde_json::Value = response2.json().await.unwrap();
    assert_eq!(body2["status"], "ok");
    assert!(body2["message"].as_str().unwrap().contains("Cleaned"));

    let _ = std::fs::remove_file(&session_path);
}

#[tokio::test]
async fn control_message_extend_lifetime() {
    let session_path =
        std::env::temp_dir().join(format!("inf-splitter-ctrl-ext-{}.toml", std::process::id()));
    let _ = std::fs::remove_file(&session_path);

    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-ext-1", "Reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0
interactions_session_store = "{store_path}"

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
control_extend_lifetime = "{CTRL_EXTEND}"
"#,
        store_path = session_path.display(),
        CTRL_EXTEND = CTRL_EXTEND,
    );
    let proxy_addr = spawn_router(&config).await;

    // First establish a session with a normal message
    let request1 = make_anthropic_request("gemini-3.1-flash-lite");
    let response1 = post_anthropic_with_session(
        &proxy_addr,
        serde_json::to_value(&request1).unwrap(),
        "sess-extend",
    )
    .await;
    assert_eq!(response1.status(), reqwest::StatusCode::OK);

    let future_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 86400;
    let extend_msg = CTRL_EXTEND.replace("<unix_utc>", &future_ts.to_string());

    let extend_request = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": extend_msg}
        ]
    });
    let response = post_anthropic_with_session(&proxy_addr, extend_request, "sess-extend").await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["message"].as_str().unwrap().contains("extended"));

    let _ = std::fs::remove_file(&session_path);
}

#[tokio::test]
async fn control_message_idempotency() {
    let session_path = std::env::temp_dir().join(format!(
        "inf-splitter-ctrl-idem-{}.toml",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&session_path);

    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-idem-1", "Reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0
interactions_session_store = "{store_path}"

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
control_clean_all = "{CTRL_CLEAN_ALL}"
"#,
        store_path = session_path.display(),
        CTRL_CLEAN_ALL = CTRL_CLEAN_ALL,
    );
    let proxy_addr = spawn_router(&config).await;

    let clean_request = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": CTRL_CLEAN_ALL}
        ]
    });
    // First: should be processed
    let response1 =
        post_anthropic_with_session(&proxy_addr, clean_request.clone(), "sess-idem").await;
    assert_eq!(response1.status(), reqwest::StatusCode::OK);
    let body1: serde_json::Value = response1.json().await.unwrap();
    assert_eq!(body1["status"], "ok");

    // Second: same message — idempotent, should be ignored
    let response2 = post_anthropic_with_session(&proxy_addr, clean_request, "sess-idem").await;
    assert_eq!(response2.status(), reqwest::StatusCode::OK);
    let body2: serde_json::Value = response2.json().await.unwrap();
    assert_eq!(body2["status"], "ok");

    let _ = std::fs::remove_file(&session_path);
}

#[tokio::test]
async fn control_messages_stripped_from_delta() {
    let session_path = std::env::temp_dir().join(format!(
        "inf-splitter-ctrl-strip-{}.toml",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&session_path);

    let captured = Arc::new(Mutex::new(None::<serde_json::Value>));
    let upstream_addr = spawn_upstream(
        "/v1beta/interactions",
        captured.clone(),
        interactions_upstream_response("int-strip-1", "First reply"),
    )
    .await;

    let config = format!(
        r#"
listen_port = 0
interactions_session_store = "{store_path}"

[gemini]
endpoint_interactions = "http://{upstream_addr}/v1beta/interactions"
models = "gemini-3.1-flash-lite"
control_extend_lifetime = "{CTRL_EXTEND}"
"#,
        store_path = session_path.display(),
        CTRL_EXTEND = CTRL_EXTEND,
    );
    let proxy_addr = spawn_router(&config).await;

    // First establish a session with a normal message
    let request1 = make_anthropic_request("gemini-3.1-flash-lite");
    let response1 = post_anthropic_with_session(
        &proxy_addr,
        serde_json::to_value(&request1).unwrap(),
        "sess-strip",
    )
    .await;
    assert_eq!(response1.status(), reqwest::StatusCode::OK);

    // Send a request with ONLY a control message — it should be intercepted
    // and return the extend-lifetime action response (not forwarded to upstream)
    let extend_msg = CTRL_EXTEND.replace(
        "<unix_utc>",
        &(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 86400)
            .to_string(),
    );
    let request = serde_json::json!({
        "model": "gemini-3.1-flash-lite",
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": extend_msg}
        ]
    });
    let response = post_anthropic_with_session(&proxy_addr, request, "sess-strip").await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["message"].as_str().unwrap().contains("extended"));

    let _ = std::fs::remove_file(&session_path);
}
