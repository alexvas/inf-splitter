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

/// Poll a file until it exists and has non-empty content, with a timeout.
pub async fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.trim().is_empty() {
                return content;
            }
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for diagnostics file: {}", path.display());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
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

async fn bind_and_serve(app: Router) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve upstream");
    });
    (addr, handle)
}
