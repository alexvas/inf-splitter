use axum::http::HeaderMap;
use reqwest::RequestBuilder;

/// Forward non-hop-by-hop headers to upstream, with optional auth override.
///
/// When `api_key` is set, auth headers are replaced with the configured key.
/// When `api_key` is None, incoming auth headers are forwarded as-is.
/// Non-auth request headers (e.g. `x-request-id`) are always forwarded.
pub fn forward_request_headers(
    mut builder: RequestBuilder,
    headers: &HeaderMap,
    api_key: Option<&str>,
) -> RequestBuilder {
    if let Some(key) = api_key {
        builder = builder
            .header("x-api-key", key)
            .header("Authorization", format!("Bearer {key}"));
    }

    for (name, value) in headers.iter() {
        let name_str = name.as_str();
        if !should_forward_request_header(name_str) {
            continue;
        }
        if api_key.is_some() && is_auth_header(name_str) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            builder = builder.header(name_str, value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use reqwest::Client;

    #[test]
    fn is_auth_header_detects_variants() {
        assert!(is_auth_header("x-api-key"));
        assert!(is_auth_header("X-API-KEY"));
        assert!(is_auth_header("Authorization"));
        assert!(is_auth_header("authorization"));
        assert!(!is_auth_header("content-type"));
        assert!(!is_auth_header("x-request-id"));
    }

    #[test]
    fn should_forward_excludes_hop_by_hop() {
        assert!(!should_forward_request_header("host"));
        assert!(!should_forward_request_header("Connection"));
        assert!(!should_forward_request_header("Transfer-Encoding"));
        assert!(!should_forward_request_header("Content-Length"));
        assert!(!should_forward_request_header("Keep-Alive"));
        assert!(should_forward_request_header("x-request-id"));
        assert!(should_forward_request_header("content-type"));
        assert!(should_forward_request_header("accept"));
    }

    #[test]
    fn forward_request_headers_applies_auth_override() {
        let client = Client::new();
        let builder = client.get("http://example.com");

        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("incoming-key"));
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer incoming-token"),
        );

        let result = forward_request_headers(builder, &headers, Some("override-key"));
        let req = result.build().expect("build request");

        assert_eq!(req.headers().get("x-api-key").unwrap(), "override-key");
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer override-key"
        );
    }

    #[test]
    fn forward_request_headers_forwards_incoming_auth_when_no_override() {
        let client = Client::new();
        let builder = client.get("http://example.com");

        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("incoming-key"));

        let result = forward_request_headers(builder, &headers, None);
        let req = result.build().expect("build request");

        assert_eq!(req.headers().get("x-api-key").unwrap(), "incoming-key");
    }

    #[test]
    fn forward_request_headers_forwards_non_auth_headers() {
        let client = Client::new();
        let builder = client.get("http://example.com");

        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("req-123"));

        let result = forward_request_headers(builder, &headers, Some("key"));
        let req = result.build().expect("build request");

        assert_eq!(req.headers().get("x-request-id").unwrap(), "req-123");
    }
}
