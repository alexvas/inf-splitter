pub mod auth;
pub mod config;
pub mod error;
pub mod local;
pub mod remote;
pub mod router;

use std::sync::Arc;

use axum::Router;

use crate::config::Config;
use crate::error::AppError;
use crate::local::OpenAiHandler;
use crate::remote::AnthropicHandler;
use crate::router::{router, AppState};

pub async fn build_app(config: Config) -> Result<Router, AppError> {
    let config = Arc::new(config);
    let openai = OpenAiHandler::new(config.as_ref())?;
    let anthropic = AnthropicHandler::new();
    Ok(router(AppState {
        config,
        openai,
        anthropic,
    }))
}
