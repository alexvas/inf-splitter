# Delta: Diagnostics

**Change ID:** `fix-unfinalized-guard-and-ct-leak`
**Affects:** `RequestDiagnostics` API, `should_forward_request_header`

---

## ADDED

### Requirement: `abort_*` Error Finalization Methods

`RequestDiagnostics` provides three public methods that finalize the guard with an error and return the appropriate `AppError` variant for `?` propagation:

- `abort_upstream(duration_ms, request_size, upstream, direction, streaming, error) → AppError::Upstream` — HTTP 502 Bad Gateway
- `abort_internal(duration_ms, request_size, upstream, direction, streaming, error) → AppError::Internal` — HTTP 500 Internal Server Error
- `abort_bad_request(duration_ms, request_size, upstream, direction, streaming, error) → AppError::BadRequest` — HTTP 400 Bad Request

All three delegate to a private `abort_with()` helper that calls `finish_with_error(status, ...)` then constructs the correct `AppError` variant through a closure.

**Design rationale:** The `status` parameter is removed from the public API — each variant maps to exactly one HTTP status code, eliminating the possibility of passing a mismatched status.

#### Scenario: Network error in handler

- GIVEN a handler function owns a `RequestDiagnostics` guard
- AND `builder.body(bytes).send().await` returns `Err(reqwest::Error)`
- WHEN the error is mapped via `.map_err(|e| guard.abort_upstream(duration_ms, request_size, upstream, direction, streaming, e))?`
- THEN `guard.finish_with_error(502, ...)` is called with the error message
- AND `AppError::Upstream(msg)` is returned and propagated via `?`
- AND the guard is finalized BEFORE the error leaves the function

#### Scenario: Serialization failure in handler

- GIVEN a handler function owns a guard
- AND `serde_json::to_vec(&value)` returns `Err(serde_json::Error)`
- WHEN the error is mapped via `.map_err(|e| guard.abort_internal(0, request_size, upstream, direction, false, e))?`
- THEN `guard.finish_with_error(500, ...)` is called
- AND `AppError::Internal(msg)` is returned

#### Scenario: Validation failure in split path

- GIVEN `handle_split_send` checks `can_split_under_limit`
- AND the function returns `Err`
- WHEN the error is handled
- THEN `guard.abort_bad_request(0, body_len, upstream, direction, stream, "request cannot be split under proxy limit")` is called
- AND `AppError::BadRequest(msg)` is returned

#### Scenario: Abort is idempotent

- GIVEN `guard.abort_upstream(...)` was already called for an earlier chunk in a split loop
- WHEN a subsequent chunk fails and `guard.abort_upstream(...)` is called again
- THEN the second call is a no-op (guard already finalized)
- AND no panic or double-record occurs

---

## MODIFIED

### Requirement: `finish_with_error` Visibility

`finish_with_error` is changed from `pub` to private (`fn` without visibility modifier). It is now only called from:
- `abort_with()` — the private helper for all `abort_*` methods
- `finish_with_upstream_error()` — which adds a response dump before delegating

External code must use one of `abort_upstream`, `abort_internal`, `abort_bad_request`, or `finish_with_upstream_error` to finalize a guard with error.

#### Scenario: External code cannot call finish_with_error directly

- GIVEN code outside `diagnostics.rs`
- WHEN attempting to call `guard.finish_with_error(...)`
- THEN the compiler rejects the call (private method)
- AND the developer must choose an appropriate `abort_*` or `finish_with_upstream_error` instead

### Requirement: No Unfinalized Guard on Error Return (Updated)

The invariant is strengthened: in addition to the existing rule (every `?` must have the guard finalized before propagation), the preferred pattern is `.map_err(|e| guard.abort_upstream(...))?` / `.map_err(|e| guard.abort_internal(...))?` / `.map_err(|e| guard.abort_bad_request(...))?`. This replaces the previous `match` + `guard.finish_with_error()` + `return Err(...)` pattern with a single expression.

The old pattern `guard.finish_with_error(status, ...)` + `return Err(AppError::Something(msg))` is superseded by `return Err(guard.abort_xxx(duration_ms, ...))`.

### Requirement: Streaming Error Finalization

Inside `tokio::spawn` blocks where `?` propagation is not possible, `let _ = guard.abort_upstream(...)` is used instead of `guard.finish_with_error(502, ...)` for consistency with the non-spawned error paths.

---

## ADDED (auth.rs)

### Requirement: `content-type` Excluded from Request Header Forwarding

`should_forward_request_header()` excludes `content-type` alongside other hop-by-hop headers (`connection`, `transfer-encoding`, etc.). Handlers set `Content-Type: application/json` explicitly on the outgoing request builder; forwarding the ingress value caused duplicate headers.

#### Scenario: Content-Type is not forwarded to upstream

- GIVEN an ingress request with `Content-Type: application/json`
- AND `forward_request_headers_map()` is called to build egress headers
- WHEN the resulting headers are inspected
- THEN `content-type` is absent from the output
- AND the explicit `Content-Type: application/json` set by each handler is the sole Content-Type header on the wire
