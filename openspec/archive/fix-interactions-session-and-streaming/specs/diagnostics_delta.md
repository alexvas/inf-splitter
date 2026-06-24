# Delta: Diagnostics

**Change ID:** `fix-interactions-session-and-streaming`
**Affects:** `src/lib.rs`, `src/interactions_handler.rs`

---

## MODIFIED

### Requirement: Shared Non-UTF-8 Upstream Body Validation

`validate_upstream_body` now records a base64-encoded dump of the non-UTF-8 body **before** returning `Err`, so operators can debug upstream failures. Previously the body was rejected without any diagnostic record.

The function signature is unchanged. On failure, the body bytes are encoded as base64 and recorded via `tracing::warn!` with the base64 payload, and a dump event is emitted if diagnostics are active.

#### Scenario: Binary upstream response recorded before rejection
- GIVEN upstream returns bytes `0xFF 0xFE 0x00` with `content-type: application/json`
- WHEN `validate_upstream_body` is called
- THEN the body is base64-encoded
- AND `tracing::warn!("non-utf8 upstream response body, base64: {}", encoded)` is emitted
- AND if diagnostics `dump_mode` is `"all"` or `"error"`, a dump event is recorded with `body: { "encoding": "base64", "data": "...base64..." }`
- AND `AppError::Internal("non-utf8 response from upstream")` is returned

#### Scenario: Valid UTF-8 passes through (unchanged)
- GIVEN upstream returns valid UTF-8 JSON bytes
- WHEN `validate_upstream_body` is called
- THEN `Ok(ValidatedBody { text, dump })` is returned (no change)

---

### Requirement: Every Protocol Handler Records Dump Events

The interactions non-split success path now records an `ingress_response_dump` — the **translated** response body sent to the client — matching the split-path behavior. Previously only the raw upstream response was dumped; the client-facing translated body was absent from diagnostics.

#### Scenario: Non-split interactions success records ingress response dump
- GIVEN `dump_mode = "all"` and a non-split interactions request completes successfully
- WHEN the translated response is sent to the client
- THEN the dump output contains four entries for the same `request_id`: `ingress/request`, `egress/request`, `egress/response`, and `ingress/response`
- AND the `ingress/response` body is the translated response body (Anthropic `MessageResponse` or OpenAI `ChatCompletionResponse`)
- AND the `ingress/response` `stage` is `"ingress"` and `direction` is `"response"`

#### Scenario: Split-send already records ingress response dump (unchanged)
- GIVEN a split-send interactions request
- WHEN the final translated response is sent
- THEN `ingress_response_dump` is recorded (existing behavior, no regression)

---

### Requirement: RequestDiagnostics Session Guard (v2)

No change to the guard struct or its methods. The guard already supports deferred dump recording; the fixes in this change use existing guard methods correctly without API changes.

---

## ADDED

(None)

---

## REMOVED

(None)
