use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde::Serialize;

use crate::config::{Config, Protocol};
use crate::error::AppError;
use crate::local::OpenAiHandler;
use crate::remote::AnthropicHandler;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub openai: OpenAiHandler,
    pub anthropic: AnthropicHandler,
}

#[derive(Debug, Deserialize)]
struct MessagePeek {
    model: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct ModelObject {
    #[serde(rename = "type")]
    pub model_type: String,
    pub id: String,
    pub display_name: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelsListResponse {
    pub data: Vec<ModelObject>,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub has_more: bool,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/openai/v1/messages", post(post_openai_messages))
        .route("/anthropic/v1/messages", post(post_anthropic_messages))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

pub fn build_models_response(config: &Config) -> ModelsListResponse {
    let ids = config.sorted_model_ids();
    let data: Vec<ModelObject> = ids
        .iter()
        .map(|id| ModelObject {
            model_type: "model".to_string(),
            id: id.clone(),
            display_name: id.clone(),
            created_at: "2024-01-01T00:00:00.000Z".to_string(),
        })
        .collect();
    let first_id = data.first().map(|model| model.id.clone());
    let last_id = data.last().map(|model| model.id.clone());
    ModelsListResponse {
        data,
        first_id,
        last_id,
        has_more: false,
    }
}

async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    Json(build_models_response(&state.config))
}

async fn post_openai_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    dispatch_messages(&state, Protocol::OpenAi, headers, body).await
}

async fn post_anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    dispatch_messages(&state, Protocol::Anthropic, headers, body).await
}

async fn dispatch_messages(
    state: &AppState,
    ingress: Protocol,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let peek: MessagePeek = serde_json::from_slice(&body)
        .map_err(|err| AppError::BadRequest(format!("invalid JSON body: {err}")))?;

    if peek.model.trim().is_empty() {
        return Err(AppError::BadRequest("model must not be empty".to_string()));
    }

    let route = state
        .config
        .resolve_route(&peek.model)
        .map_err(AppError::from)?;

    if ingress != route.protocol {
        tracing::debug!(
            model = %peek.model,
            ingress = %ingress,
            target = %route.protocol,
            "converting request to target protocol"
        );
    }

    tracing::debug!(
        model = %peek.model,
        section = %route.section,
        endpoint = %route.endpoint,
        protocol = %route.protocol,
        "routing request"
    );

    match (ingress, route.protocol) {
        (Protocol::Anthropic, Protocol::Anthropic) => {
            state
                .anthropic
                .handle_from_anthropic(body, &headers, &route)
                .await
        }
        (Protocol::Anthropic, Protocol::OpenAi) => {
            state
                .openai
                .handle_from_anthropic(&body, &headers, &route)
                .await
        }
        (Protocol::OpenAi, Protocol::OpenAi) => {
            state
                .openai
                .handle_from_openai(&body, &headers, &route)
                .await
        }
        (Protocol::OpenAi, Protocol::Anthropic) => {
            state
                .anthropic
                .handle_from_openai(&body, &headers, &route)
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(model_routes: &[(&str, &str)]) -> Config {
        let model_routes = model_routes
            .iter()
            .map(|(model, section)| ((*model).to_string(), (*section).to_string()))
            .collect();
        Config::from_model_routes(model_routes)
    }

    #[test]
    fn models_response_has_anthropic_shape() {
        let config = test_config(&[
            ("gemma4:31b", "ollama"),
            ("deepseek-v4-pro[1m]", "deepseek"),
        ]);
        let response = build_models_response(&config);

        assert_eq!(response.has_more, false);
        assert_eq!(response.data.len(), 2);
        for model in &response.data {
            assert_eq!(model.model_type, "model");
            assert_eq!(model.id, model.display_name);
            assert!(!model.created_at.is_empty());
        }
        assert_eq!(response.first_id.as_deref(), Some("deepseek-v4-pro[1m]"));
        assert_eq!(response.last_id.as_deref(), Some("gemma4:31b"));
    }

    #[test]
    fn models_response_order_is_lexicographic_and_stable() {
        let config = test_config(&[
            ("gemma4:31b", "ollama"),
            ("llama3:8b", "ollama"),
            ("deepseek-v4-flash", "deepseek"),
            ("deepseek-v4-pro[1m]", "deepseek"),
        ]);
        let first = build_models_response(&config);
        let second = build_models_response(&config);

        let ids: Vec<&str> = first.data.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "deepseek-v4-flash",
                "deepseek-v4-pro[1m]",
                "gemma4:31b",
                "llama3:8b"
            ]
        );
        assert_eq!(first, second);
    }
}
