use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::stream::Stream;

use crate::diagnostics::Diagnostics;

pub(crate) const MAX_STREAMING_DUMP_BYTES: usize = 1024 * 1024;

pub(crate) struct RelayContext<'a> {
    pub(crate) diagnostics: &'a Diagnostics,
    pub(crate) request_id: String,
    pub(crate) model: String,
    pub(crate) section: String,
}

pub(crate) struct DiagnosticStream<S> {
    pub(crate) inner: S,
    pub(crate) buffer: Vec<u8>,
    pub(crate) diagnostics: Diagnostics,
    pub(crate) request_id: String,
    pub(crate) section: String,
    pub(crate) model: String,
    pub(crate) response_headers: Vec<(String, String)>,
    pub(crate) status: u16,
    pub(crate) dumped: bool,
}

impl<S> Stream for DiagnosticStream<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                if self.diagnostics.dump_enabled() && self.buffer.len() < MAX_STREAMING_DUMP_BYTES {
                    let remaining = MAX_STREAMING_DUMP_BYTES - self.buffer.len();
                    let to_take = std::cmp::min(chunk.len(), remaining);
                    self.buffer.extend_from_slice(&chunk[..to_take]);
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
                if !self.dumped {
                    self.dumped = true;
                    let body = crate::diagnostics::dump_body_from_bytes(&self.buffer);
                    if body.is_base64() {
                        tracing::warn!(
                            request_id = %self.request_id,
                            direction = "response",
                            body_len = self.buffer.len(),
                            "non-utf8 streaming upstream response"
                        );
                    }
                    let headers = std::mem::take(&mut self.response_headers);
                    self.diagnostics.record_response_dump(
                        &self.request_id,
                        &self.section,
                        &self.model,
                        headers,
                        body,
                        self.status,
                        false,
                    );
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> Drop for DiagnosticStream<S> {
    fn drop(&mut self) {
        if self.dumped || !self.diagnostics.dump_enabled() || self.buffer.is_empty() {
            return;
        }
        let body = crate::diagnostics::dump_body_from_bytes(&self.buffer);
        if body.is_base64() {
            tracing::warn!(
                request_id = %self.request_id,
                direction = "response",
                body_len = self.buffer.len(),
                "non-utf8 streaming upstream response (dropped before EOF)"
            );
        }
        let headers = std::mem::take(&mut self.response_headers);
        self.diagnostics.record_response_dump(
            &self.request_id,
            &self.section,
            &self.model,
            headers,
            body,
            self.status,
            false,
        );
    }
}

/// Apply per-route token caps to an OpenAI `ChatCompletionRequest`.
///
/// Only for **OpenAI egress paths**: passthrough to an OpenAI upstream, or
/// Anthropic-to-OpenAI translation. Mutates the request in place, clamping or
/// setting `max_tokens`, `max_completion_tokens`, and `max_output_tokens`
/// (via `extra`) to the route's configured limits.
pub(crate) fn cap_openai_max_tokens(
    req: &mut anyllm_translate::openai::ChatCompletionRequest,
    route: &crate::config::RouteTarget,
) {
    if let Some(limit) = route.max_tokens {
        match req.max_tokens {
            Some(existing) if existing > limit => req.max_tokens = Some(limit),
            None => req.max_tokens = Some(limit),
            _ => {}
        }
    }
    if let Some(limit) = route.max_completion_tokens {
        match req.max_completion_tokens {
            Some(existing) if existing > limit => req.max_completion_tokens = Some(limit),
            None => req.max_completion_tokens = Some(limit),
            _ => {}
        }
    }
    if let Some(limit) = route.max_output_tokens {
        match req.extra.get("max_output_tokens").and_then(|v| v.as_u64()) {
            Some(existing) if existing > limit as u64 => {
                req.extra
                    .insert("max_output_tokens".to_string(), serde_json::json!(limit));
            }
            None => {
                req.extra
                    .insert("max_output_tokens".to_string(), serde_json::json!(limit));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_route() -> crate::config::RouteTarget {
        crate::config::RouteTarget {
            section: "test".into(),
            ..Default::default()
        }
    }

    fn make_openai_req() -> anyllm_translate::openai::ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .unwrap()
    }

    #[test]
    fn cap_openai_max_tokens_sets_missing() {
        let mut req = make_openai_req();
        let mut route = empty_route();
        route.max_tokens = Some(1024);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_tokens, Some(1024));
    }

    #[test]
    fn cap_openai_max_tokens_clamps_exceeding() {
        let mut req = make_openai_req();
        req.max_tokens = Some(4096);
        let mut route = empty_route();
        route.max_tokens = Some(1024);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_tokens, Some(1024));
    }

    #[test]
    fn cap_openai_max_tokens_leaves_below() {
        let mut req = make_openai_req();
        req.max_tokens = Some(512);
        let mut route = empty_route();
        route.max_tokens = Some(1024);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_tokens, Some(512));
    }

    #[test]
    fn cap_openai_max_completion_tokens_sets_missing() {
        let mut req = make_openai_req();
        let mut route = empty_route();
        route.max_completion_tokens = Some(2048);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_completion_tokens, Some(2048));
    }

    #[test]
    fn cap_openai_max_output_tokens_sets_missing_via_extra() {
        let mut req = make_openai_req();
        let mut route = empty_route();
        route.max_output_tokens = Some(500);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(
            req.extra.get("max_output_tokens").and_then(|v| v.as_u64()),
            Some(500)
        );
    }

    #[test]
    fn cap_openai_max_output_tokens_clamps_exceeding() {
        let mut req = make_openai_req();
        req.extra
            .insert("max_output_tokens".into(), serde_json::json!(1000u64));
        let mut route = empty_route();
        route.max_output_tokens = Some(500);
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(
            req.extra.get("max_output_tokens").and_then(|v| v.as_u64()),
            Some(500)
        );
    }

    #[test]
    fn cap_openai_no_limits_leaves_unchanged() {
        let mut req = make_openai_req();
        req.max_tokens = Some(4096);
        let route = empty_route();
        cap_openai_max_tokens(&mut req, &route);
        assert_eq!(req.max_tokens, Some(4096));
    }
}
