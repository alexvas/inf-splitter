mod common;

use std::sync::{Arc, Mutex};

use anyllm_translate::anthropic::{Content, MessageCreateRequest};
use anyllm_translate::openai::{ChatCompletionRequest, ChatContent};
use common::{
    anthropic_upstream_response, openai_upstream_response, spawn_error_upstream, spawn_router,
    spawn_upstream,
};

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
