use axum::http::HeaderMap;
use reqwest::RequestBuilder;

/// Apply upstream auth: section `api_key` overrides incoming headers when set.
pub fn apply_upstream_auth(
    mut builder: RequestBuilder,
    request_headers: &HeaderMap,
    api_key: Option<&str>,
) -> RequestBuilder {
    if let Some(key) = api_key {
        return builder
            .header("x-api-key", key)
            .header("Authorization", format!("Bearer {key}"));
    }

    for (name, value) in request_headers.iter() {
        if is_auth_header(name.as_str()) {
            if let Ok(value) = value.to_str() {
                builder = builder.header(name.as_str(), value);
            }
        }
    }

    builder
}

pub fn is_auth_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "x-api-key" | "authorization"
    )
}

pub fn should_forward_request_header(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "te"
            | "trailers"
            | "upgrade"
            | "keep-alive"
    )
}
