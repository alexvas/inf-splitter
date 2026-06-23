use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use inf_splitter::config::Config;
use inf_splitter::diagnostics::{DiagnosticMode, Diagnostics, DiagnosticsConfig};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

#[derive(Clone)]
struct CaptureState {
    captured: Arc<Mutex<Option<serde_json::Value>>>,
    response: serde_json::Value,
}

pub fn openai_upstream_response(model: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 5,
            "total_tokens": 8
        }
    })
}

pub fn anthropic_upstream_response(model: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 3, "output_tokens": 5}
    })
}

pub fn interactions_upstream_response(id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "status": "completed",
        "created": "2026-01-01T00:00:00Z",
        "updated": "2026-01-01T00:00:00Z",
        "steps": [{
            "type": "model_output",
            "content": [{"type": "text", "text": text}]
        }],
        "usage": {"total_input_tokens": 5, "total_output_tokens": 10}
    })
}

/// Send a POST to `/v1/chat/completions` on the proxy with a JSON body.
pub async fn post_openai(proxy_addr: &SocketAddr, body: serde_json::Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .expect("proxy request")
}

/// Send a POST to `/v1/messages` on the proxy with a JSON body.
pub async fn post_anthropic(proxy_addr: &SocketAddr, body: serde_json::Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/messages"))
        .json(&body)
        .send()
        .await
        .expect("proxy request")
}

pub async fn spawn_upstream(
    path: &'static str,
    captured: Arc<Mutex<Option<serde_json::Value>>>,
    response: serde_json::Value,
) -> SocketAddr {
    let state = CaptureState { captured, response };
    let app = Router::new()
        .route(path, post(capture_and_respond))
        .with_state(state);
    bind_and_serve(app).await.0
}

/// Like `spawn_upstream` but captures ALL request bodies (not just the last).
/// Useful for split-send tests where multiple chunks are sent.
pub async fn spawn_upstream_capture_all(
    path: &'static str,
    captured: Arc<Mutex<Vec<serde_json::Value>>>,
    response: serde_json::Value,
) -> SocketAddr {
    #[derive(Clone)]
    struct AllCaptureState {
        captured: Arc<Mutex<Vec<serde_json::Value>>>,
        response: serde_json::Value,
    }
    async fn capture_all(
        State(state): State<AllCaptureState>,
        Json(body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        state.captured.lock().expect("lock captured").push(body);
        Json(state.response.clone())
    }
    let state = AllCaptureState { captured, response };
    let app = Router::new()
        .route(path, post(capture_all))
        .with_state(state);
    bind_and_serve(app).await.0
}

pub async fn spawn_router(config_toml: &str) -> SocketAddr {
    let mut config = Config::load_from_str(config_toml).expect("test config");
    config.listen_addr = "127.0.0.1:0".parse().expect("ephemeral listen addr");

    let diagnostics = Diagnostics::new(DiagnosticsConfig::default());
    let app = inf_splitter::build_app(config, diagnostics)
        .await
        .expect("build proxy app");
    bind_and_serve(app).await.0
}

/// Like `spawn_router` but with `stats_mode = "error"` in the diagnostics config.
pub async fn spawn_router_with_dump(config_toml: &str) -> SocketAddr {
    let mut config = Config::load_from_str(config_toml).expect("test config");
    config.listen_addr = "127.0.0.1:0".parse().expect("ephemeral listen addr");

    let diag_config = DiagnosticsConfig {
        stats_mode: DiagnosticMode::Error,
        ..DiagnosticsConfig::default()
    };
    let diagnostics = Diagnostics::new(diag_config);
    let app = inf_splitter::build_app(config, diagnostics)
        .await
        .expect("build proxy app");
    bind_and_serve(app).await.0
}

/// Spawn router with a fully custom `DiagnosticsConfig`.
pub async fn spawn_router_with_diagnostics(
    config_toml: &str,
    diag_config: DiagnosticsConfig,
) -> SocketAddr {
    let mut config = Config::load_from_str(config_toml).expect("test config");
    config.listen_addr = "127.0.0.1:0".parse().expect("ephemeral listen addr");

    let diagnostics = Diagnostics::new(diag_config);
    let app = inf_splitter::build_app(config, diagnostics)
        .await
        .expect("build proxy app");
    bind_and_serve(app).await.0
}

/// Spawn an upstream that returns a fixed HTTP status and JSON body.
pub async fn spawn_error_upstream(
    path: &'static str,
    status: StatusCode,
    body: serde_json::Value,
) -> SocketAddr {
    let app = Router::new().route(
        path,
        post(move |Json(_): Json<serde_json::Value>| async move {
            (status, Json(body)).into_response()
        }),
    );
    bind_and_serve(app).await.0
}

/// Spawn an upstream that sleeps for `delay` before returning a fixed JSON
/// response.  Useful for verifying that `duration_ms` timing is wired up.
pub async fn spawn_delayed_upstream(
    path: &'static str,
    delay: Duration,
    response: serde_json::Value,
) -> SocketAddr {
    let app = Router::new().route(
        path,
        post(move |Json(_): Json<serde_json::Value>| async move {
            sleep(delay).await;
            Json(response)
        }),
    );
    bind_and_serve(app).await.0
}

async fn capture_and_respond(
    State(state): State<CaptureState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    *state.captured.lock().expect("lock captured request") = Some(body);
    Json(state.response.clone())
}

/// Spawn an upstream that returns a fixed SSE stream (content-type text/event-stream).
pub async fn spawn_stream_upstream(path: &'static str, sse_body: String) -> SocketAddr {
    use axum::body::Body;
    use axum::http::{header, StatusCode};
    use axum::response::Response;

    let app = Router::new().route(
        path,
        post(move || async move {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from(sse_body.clone()))
                .unwrap()
        }),
    );
    bind_and_serve(app).await.0
}

pub async fn poll_diagnostics_file(
    path: &std::path::Path,
    deadline_msg: &str,
    pred: impl Fn(&str) -> bool,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let content = loop {
        if let Ok(c) = std::fs::read_to_string(path) {
            if pred(&c) {
                // Content satisfies predicate — wait a bit for the writer
                // to flush any remaining deferred dumps, then re-read.
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                if let Ok(c2) = std::fs::read_to_string(path) {
                    if c2.len() == c.len() {
                        break c2;
                    }
                }
            }
        }
        if tokio::time::Instant::now() > deadline {
            panic!("{deadline_msg}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    let _ = std::fs::remove_file(path);
    content.trim().lines().map(String::from).collect()
}

/// Poll a file until it exists and has non-empty content, with a timeout.
/// Removes the file before returning its lines.
pub async fn wait_for_file(path: &std::path::Path) -> Vec<String> {
    poll_diagnostics_file(
        path,
        &format!("timed out waiting for diagnostics file: {}", path.display()),
        |c| !c.trim().is_empty(),
    )
    .await
}

/// Poll a diagnostics dump file until it contains at least one ingress
/// line (stage="ingress"). Removes the file before returning its lines.
pub async fn wait_for_ingress_dump(path: &std::path::Path) -> Vec<String> {
    wait_for_stage(path, "ingress", None).await
}

/// Poll a diagnostics dump file until it contains at least one egress
/// line (stage="egress"). Removes the file before returning its lines.
pub async fn wait_for_egress_dump(path: &std::path::Path) -> Vec<String> {
    wait_for_stage(path, "egress", None).await
}

/// Poll a diagnostics dump file until it contains at least one egress
/// response line (stage="egress", direction="response"). Removes the file
/// before returning its lines.
pub async fn wait_for_egress_response_dump(path: &std::path::Path) -> Vec<String> {
    wait_for_stage(path, "egress", Some("response")).await
}

async fn wait_for_stage(
    path: &std::path::Path,
    stage: &str,
    direction: Option<&str>,
) -> Vec<String> {
    poll_diagnostics_file(
        path,
        &format!(
            "timed out waiting for {} dump in: {}",
            match direction {
                Some(dir) => format!("stage={stage} direction={dir}"),
                None => format!("stage={stage}"),
            },
            path.display()
        ),
        move |c| {
            c.lines().any(|line| {
                serde_json::from_str(line)
                    .ok()
                    .map_or(false, |v: serde_json::Value| {
                        v["stage"].as_str() == Some(stage)
                            && direction.map_or(true, |d| v["direction"].as_str() == Some(d))
                    })
            })
        },
    )
    .await
}

/// Spawn an upstream that returns an SSE stream with custom headers.
pub async fn spawn_sse_upstream_with_headers(
    path: &'static str,
    sse_body: String,
    extra_headers: Vec<(&'static str, String)>,
) -> SocketAddr {
    use axum::body::Body;
    use axum::http::header;
    use axum::http::StatusCode;
    use axum::response::Response;

    let app = Router::new().route(
        path,
        post(move || {
            let body = sse_body.clone();
            let extra_headers = extra_headers.clone();
            async move {
                let mut builder = Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream");
                for (name, value) in &extra_headers {
                    builder = builder.header(*name, value.as_str());
                }
                builder.body(Body::from(body)).unwrap()
            }
        }),
    );
    bind_and_serve(app).await.0
}

pub(crate) async fn bind_and_serve(app: Router) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve upstream");
    });
    (addr, handle)
}
