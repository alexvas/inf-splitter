use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::anthropic::AnthropicHandler;
use crate::config::{Config, Protocol};
use crate::diagnostics::Diagnostics;
use crate::error::AppError;
use crate::openai::OpenAiHandler;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub diagnostics: Diagnostics,
    pub openai: OpenAiHandler,
    pub anthropic: AnthropicHandler,
    pub health_client: reqwest::Client,
    pub health_cache: Arc<Mutex<Option<(Instant, HealthResponse)>>>,
}

#[derive(Debug, Serialize, Clone)]
pub struct HealthResponse {
    pub status: String,
    pub upstreams: HashMap<String, String>,
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
        .route("/openai/v1/models", get(list_models))
        .route("/anthropic/v1/models", get(list_models))
        .route("/openai/v1/messages", post(post_openai_messages))
        .route("/anthropic/v1/messages", post(post_anthropic_messages))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    const CACHE_TTL: Duration = Duration::from_secs(5);
    const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

    {
        let cache = state.health_cache.lock().await;
        if let Some((cached_at, ref cached_response)) = *cache {
            if cached_at.elapsed() < CACHE_TTL {
                let status = if cached_response.status == "ok" {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                return (status, Json(cached_response.clone())).into_response();
            }
        }
    }

    let endpoints = state.config.upstream_endpoints();
    let mut upstreams = HashMap::new();
    let mut all_ok = true;

    let checks: Vec<_> = endpoints
        .iter()
        .map(|(name, endpoint)| {
            let client = state.health_client.clone();
            let endpoint = endpoint.clone();
            let name = name.clone();
            async move {
                let url = format!("{endpoint}/");
                let result = tokio::time::timeout(CHECK_TIMEOUT, client.head(&url).send()).await;
                match result {
                    Ok(Ok(_)) => (name, "ok".to_string()),
                    Ok(Err(e)) => {
                        tracing::warn!(upstream = %name, url = %url, error = ?e, "health probe failed: unreachable");
                        (name, "unreachable".to_string())
                    },
                    Err(_) => {
                        tracing::warn!(upstream = %name, url = %url, "health probe failed: timeout");
                        (name, "timeout".to_string())
                    },
                }
            }
        })
        .collect();

    for (name, status) in futures::future::join_all(checks).await {
        if status != "ok" {
            all_ok = false;
        }
        upstreams.insert(name, status);
    }

    let response = HealthResponse {
        status: if all_ok {
            "ok".to_string()
        } else {
            "degraded".to_string()
        },
        upstreams,
    };

    {
        let mut cache = state.health_cache.lock().await;
        *cache = Some((Instant::now(), response.clone()));
    }

    let status_code = if all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status_code, Json(response)).into_response()
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
    let body_str = std::str::from_utf8(&body).map_err(|_| {
        let request_id = state.diagnostics.new_request_id();
        tracing::warn!(
            request_id = %request_id,
            direction = %ingress,
            body_len = body.len(),
            "non-utf8 client request body"
        );
        state
            .diagnostics
            .record_stats(&crate::diagnostics::StatsEvent {
                request_id: request_id.clone(),
                ts: crate::diagnostics::ts_string(),
                direction: ingress.to_string(),
                model: "?".into(),
                upstream: String::new(),
                status: 400,
                duration_ms: 0,
                request_size_bytes: body.len(),
                response_size_bytes: None,
                streaming: false,
                input_messages: None,
                max_tokens: None,
                messages_detail_ingress: None,
                messages_detail_egress: None,
                error: Some("non-utf8".into()),
            });
        // Record a dump of the non-UTF8 body
        let body = crate::diagnostics::dump_body_from_bytes(&body);
        state.diagnostics.record_request_dump(
            &request_id,
            "ingress",
            "?",
            &headers,
            body,
            None,
            true,
        );
        AppError::BadRequest("non-utf8".into())
    })?;
    let peek: MessagePeek = serde_json::from_str(body_str).map_err(|err| {
        state
            .diagnostics
            .record_stats(&crate::diagnostics::StatsEvent {
                request_id: state.diagnostics.new_request_id(),
                ts: crate::diagnostics::ts_string(),
                direction: ingress.to_string(),
                model: "?".into(),
                upstream: String::new(),
                status: 400,
                duration_ms: 0,
                request_size_bytes: body.len(),
                response_size_bytes: None,
                streaming: false,
                input_messages: None,
                max_tokens: None,
                messages_detail_ingress: None,
                messages_detail_egress: None,
                error: Some(format!("invalid JSON body: {err}")),
            });
        AppError::BadRequest(format!("invalid JSON body: {err}"))
    })?;

    if peek.model.trim().is_empty() {
        state
            .diagnostics
            .record_stats(&crate::diagnostics::StatsEvent {
                request_id: state.diagnostics.new_request_id(),
                ts: crate::diagnostics::ts_string(),
                direction: ingress.to_string(),
                model: "?".into(),
                upstream: String::new(),
                status: 400,
                duration_ms: 0,
                request_size_bytes: body.len(),
                response_size_bytes: None,
                streaming: false,
                input_messages: None,
                max_tokens: None,
                messages_detail_ingress: None,
                messages_detail_egress: None,
                error: Some("model must not be empty".into()),
            });
        return Err(AppError::BadRequest("model must not be empty".to_string()));
    }

    let route = state.config.resolve_route(&peek.model).map_err(|err| {
        state
            .diagnostics
            .record_stats(&crate::diagnostics::StatsEvent {
                request_id: state.diagnostics.new_request_id(),
                ts: crate::diagnostics::ts_string(),
                direction: ingress.to_string(),
                model: peek.model.clone(),
                upstream: String::new(),
                status: 400,
                duration_ms: 0,
                request_size_bytes: body.len(),
                response_size_bytes: None,
                streaming: false,
                input_messages: None,
                max_tokens: None,
                messages_detail_ingress: None,
                messages_detail_egress: None,
                error: Some(err.to_string()),
            });
        AppError::from(err)
    })?;

    tracing::debug!(
        model = %peek.model,
        section = %route.section,
        ingress = %ingress,
        openai_endpoint = ?route.endpoint_openai,
        anthropic_endpoint = ?route.endpoint_anthropic,
        "routing request"
    );

    match ingress {
        Protocol::OpenAi => {
            if let Some(endpoint) = &route.endpoint_openai {
                state
                    .openai
                    .handle_from_openai(&body, &headers, &route, endpoint)
                    .await
            } else if let Some(endpoint) = &route.endpoint_anthropic {
                tracing::debug!("converting OpenAI ingress to Anthropic upstream");
                state
                    .anthropic
                    .handle_from_openai(&body, &headers, &route, endpoint)
                    .await
            } else {
                Err(AppError::Internal(
                    "no endpoint configured for this provider".to_string(),
                ))
            }
        }
        Protocol::Anthropic => {
            if let Some(endpoint) = &route.endpoint_anthropic {
                state
                    .anthropic
                    .handle_from_anthropic(body, &headers, &route, endpoint)
                    .await
            } else if let Some(endpoint) = &route.endpoint_openai {
                tracing::debug!("converting Anthropic ingress to OpenAI upstream");
                state
                    .openai
                    .handle_from_anthropic(&body, &headers, &route, endpoint)
                    .await
            } else {
                Err(AppError::Internal(
                    "no endpoint configured for this provider".to_string(),
                ))
            }
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
