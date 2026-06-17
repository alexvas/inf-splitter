pub mod anthropic;
pub mod auth;
pub mod config;
pub mod diagnostics;
pub mod error;
pub mod openai;
pub mod relay;
pub mod router;
pub mod sse;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::Router;
use serde_json::Value;
use tokio::sync::Mutex;
use tower_http::limit::RequestBodyLimitLayer;

use crate::anthropic::AnthropicHandler;
use crate::config::{cap_numeric_field, Config, RouteTarget};
use crate::diagnostics::Diagnostics;
use crate::error::AppError;
use crate::openai::OpenAiHandler;
use crate::router::{router, AppState};

const BODY_TOO_LARGE_HINT: &str = "Try reducing context size or splitting into smaller requests.";

/// Append a size hint to an error body when the status code indicates the
/// request was too large.
pub(crate) fn append_size_hint(
    status: StatusCode,
    body: String,
    hint_statuses: &HashSet<StatusCode>,
) -> String {
    if !hint_statuses.contains(&status) {
        return body;
    }
    if let Ok(mut value) = serde_json::from_str::<Value>(&body) {
        if let Some(Value::String(msg)) = value.pointer_mut("/error/message") {
            *msg = format!("{msg}. Try reducing context size or splitting into smaller requests.");
            return serde_json::to_string(&value).unwrap_or(body);
        }
    }
    format!("{body}. Try reducing context size or splitting into smaller requests.")
}

/// Apply token limits from the route config to a raw JSON body (passthrough path).
#[allow(dead_code)]
pub(crate) fn apply_token_caps(body: &[u8], route: &RouteTarget) -> Result<Vec<u8>, AppError> {
    let has_caps = route.max_tokens.is_some()
        || route.max_output_tokens.is_some()
        || route.max_completion_tokens.is_some();
    if !has_caps {
        return Ok(body.to_vec());
    }
    let mut value: Value =
        serde_json::from_slice(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
    apply_token_caps_to_value(&mut value, route);
    serde_json::to_vec(&value).map_err(|e| AppError::Internal(e.to_string()))
}

/// Like `apply_token_caps` but works on an already-parsed `Value`,
/// avoiding a second deserialization when the body is already in memory.
pub(crate) fn apply_token_caps_to_value(value: &mut Value, route: &RouteTarget) {
    if let Some(limit) = route.max_tokens {
        cap_numeric_field(value, "max_tokens", limit);
    }
    if let Some(limit) = route.max_output_tokens {
        cap_numeric_field(value, "max_output_tokens", limit);
    }
    if let Some(limit) = route.max_completion_tokens {
        cap_numeric_field(value, "max_completion_tokens", limit);
    }
}

/// Remove specified top-level keys from a JSON value. No-op on empty set
/// or non-object values.
pub(crate) fn drop_fields_from_value(value: &mut Value, fields: &HashSet<String>) {
    if fields.is_empty() {
        return;
    }
    if let Some(obj) = value.as_object_mut() {
        for field in fields {
            obj.remove(field.as_str());
        }
    }
}

/// Apply token caps and drop_fields to a parsed request body (passthrough path).
pub(crate) fn apply_egress_transforms(value: &mut Value, model: &str, route: &RouteTarget) {
    apply_token_caps_to_value(value, route);
    let drop_fields = route.drop_fields.for_model(model);
    drop_fields_from_value(value, &drop_fields);
}

/// Result of `prepare_egress_body` — the serialized request body ready for
/// sending, plus the Value for diagnostics.
pub(crate) struct PreparedBody {
    pub bytes: Vec<u8>,
    pub value: Value,
    pub egress_str: Option<String>,
}

/// Serialize a typed request, apply drop_fields, return bytes + diagnostics data.
/// Used by conversion paths after translation.
pub(crate) fn prepare_egress_body<T: serde::Serialize>(
    req: &T,
    model: &str,
    route: &RouteTarget,
    diagnostics: &Diagnostics,
) -> Result<PreparedBody, AppError> {
    let mut value = serde_json::to_value(req).map_err(|e| AppError::Internal(e.to_string()))?;
    let drop_fields = route.drop_fields.for_model(model);
    drop_fields_from_value(&mut value, &drop_fields);
    let bytes = serde_json::to_vec(&value).map_err(|e| AppError::Internal(e.to_string()))?;
    let egress_str = if diagnostics.dump_enabled() {
        String::from_utf8(bytes.clone()).ok()
    } else {
        None
    };
    Ok(PreparedBody {
        bytes,
        value,
        egress_str,
    })
}

/// Extract the `model` field from a JSON byte slice. Returns `"?"` on failure.
#[allow(dead_code)]
pub(crate) fn peek_model_from_json(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from))
        .unwrap_or_else(|| "?".to_string())
}

pub async fn build_app(config: Config, diagnostics: Diagnostics) -> Result<Router, AppError> {
    let hint_statuses = config.body_too_large_hint_statuses.clone();
    let config = Arc::new(config);
    let max_request_body = config.max_request_body;
    let openai = OpenAiHandler::new(config.as_ref(), diagnostics.clone(), hint_statuses.clone())?;
    let anthropic =
        AnthropicHandler::new(config.as_ref(), diagnostics.clone(), hint_statuses.clone())?;

    let health_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|err| AppError::Internal(err.to_string()))?;

    let state = AppState {
        config,
        diagnostics,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteTarget;

    fn route_with_limits(
        max_tokens: Option<u32>,
        max_output_tokens: Option<u32>,
        max_completion_tokens: Option<u32>,
    ) -> RouteTarget {
        RouteTarget {
            section: "test".into(),
            endpoint_openai: None,
            endpoint_anthropic: None,
            api_key: None,
            max_tokens,
            max_output_tokens,
            max_completion_tokens,
            model_names: std::collections::HashSet::new(),
            drop_fields: crate::config::DropFields::default(),
        }
    }

    #[test]
    fn apply_token_caps_no_limits_returns_unchanged() {
        let body = br#"{"max_tokens":4096,"model":"test"}"#;
        let route = route_with_limits(None, None, None);
        let result = apply_token_caps(body, &route).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn apply_token_caps_clamps_max_tokens() {
        let body = br#"{"max_tokens":4096,"model":"test"}"#;
        let route = route_with_limits(Some(1024), None, None);
        let result = apply_token_caps(body, &route).unwrap();
        let v: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(v["max_tokens"], 1024);
    }

    #[test]
    fn apply_token_caps_sets_missing_max_tokens() {
        let body = br#"{"model":"test"}"#;
        let route = route_with_limits(Some(1024), None, None);
        let result = apply_token_caps(body, &route).unwrap();
        let v: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(v["max_tokens"], 1024);
    }

    #[test]
    fn apply_token_caps_applies_all_three_limits() {
        let body = br#"{"max_tokens":4096,"max_output_tokens":8192,"max_completion_tokens":16384}"#;
        let route = route_with_limits(Some(1024), Some(2048), Some(4096));
        let result = apply_token_caps(body, &route).unwrap();
        let v: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(v["max_tokens"], 1024);
        assert_eq!(v["max_output_tokens"], 2048);
        assert_eq!(v["max_completion_tokens"], 4096);
    }

    // --- peek_model_from_json ---

    #[test]
    fn peek_model_extracts_field() {
        let body = br#"{"model":"gpt-4","messages":[]}"#;
        assert_eq!(peek_model_from_json(body), "gpt-4");
    }

    #[test]
    fn peek_model_returns_question_on_missing() {
        assert_eq!(peek_model_from_json(br#"{"x":1}"#), "?");
    }

    #[test]
    fn peek_model_returns_question_on_garbage() {
        assert_eq!(peek_model_from_json(b"not json"), "?");
    }

    // --- drop_fields_from_value ---

    #[test]
    fn drop_fields_removes_specified_keys() {
        let mut v: Value = serde_json::json!({"a": 1, "b": 2, "c": 3});
        let fields: HashSet<String> = HashSet::from(["a".into(), "b".into()]);
        drop_fields_from_value(&mut v, &fields);
        assert_eq!(v, serde_json::json!({"c": 3}));
    }

    #[test]
    fn drop_fields_empty_set_is_noop() {
        let mut v: Value = serde_json::json!({"a": 1});
        let fields: HashSet<String> = HashSet::new();
        drop_fields_from_value(&mut v, &fields);
        assert_eq!(v, serde_json::json!({"a": 1}));
    }

    #[test]
    fn drop_fields_nonexistent_key_is_noop() {
        let mut v: Value = serde_json::json!({"a": 1});
        let fields: HashSet<String> = HashSet::from(["nonexistent".into()]);
        drop_fields_from_value(&mut v, &fields);
        assert_eq!(v, serde_json::json!({"a": 1}));
    }
}
