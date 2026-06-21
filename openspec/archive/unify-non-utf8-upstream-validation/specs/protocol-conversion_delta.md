# Delta: Protocol Conversion — Non-UTF-8 Validation Unification

**Change ID:** `unify-non-utf8-upstream-validation`
**Affects:** `src/lib.rs`, `src/openai.rs`, `src/anthropic.rs`, `src/interactions_handler.rs`

---

## ADDED

### Requirement: Shared Non-UTF-8 Upstream Body Validation

`validate_upstream_body(body: Bytes, request_id: &str) -> Result<ValidatedBody, AppError>` in `src/lib.rs` detects non-UTF-8 upstream response bodies. On success it returns `ValidatedBody { text, dump }` with the decoded string and a `DumpBody` ready for `response_dump`. On failure it logs `tracing::warn!("non-utf8 upstream response body")` and returns `AppError::Internal`.

Used by all three handlers (`openai.rs`, `anthropic.rs`, `interactions_handler.rs`) for non-streaming responses.

#### Scenario: Binary upstream response detected
- GIVEN upstream returns bytes `0xFF 0xFE 0x00` with `content-type: application/json`
- WHEN `validate_upstream_body` is called
- THEN `tracing::warn!` is emitted
- AND `AppError::Internal("non-utf8 response from upstream")` is returned

#### Scenario: Valid UTF-8 passes through
- GIVEN upstream returns valid UTF-8 JSON bytes
- WHEN `validate_upstream_body` is called
- THEN `Ok(ValidatedBody { text, dump })` is returned

---

## MODIFIED

### Requirement: Interactions Streaming Non-UTF-8 Protection

The interactions streaming path now rejects non-UTF-8 chunks with an SSE error event instead of silently replacing invalid bytes with `U+FFFD` via `from_utf8_lossy`.

#### Scenario: Binary chunk in interactions stream
- GIVEN an interactions SSE stream with a non-UTF-8 chunk
- WHEN the chunk is received by `handle_stream_response`
- THEN an SSE `error` event `{"type":"error","error":{"type":"upstream_error","message":"non-utf8 response from upstream"}}` is sent
- AND the stream is aborted with `finish_with_error(502, ...)`
