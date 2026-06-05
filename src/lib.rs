pub mod auth;
pub mod config;
pub mod error;
pub mod local;
pub mod remote;
pub mod router;
pub mod sse;

use std::sync::Arc;
use std::time::Duration;

use axum::http::header;
use axum::response::{IntoResponse, Json, Response};
use axum::Router;
use tokio::sync::Mutex;
use tower_http::limit::RequestBodyLimitLayer;

use crate::config::Config;
use crate::error::AppError;
use crate::local::OpenAiHandler;
use crate::remote::AnthropicHandler;
use crate::router::{router, AppState};

const BODY_TOO_LARGE_HINT: &str = "Try reducing context size or splitting into smaller requests.";

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
