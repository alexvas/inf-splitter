# Delta: Diagnostics

**Change ID:** `fix-conversion-error-response-dumps`
**Affects:** `src/diagnostics.rs`, `src/openai.rs`, `src/anthropic.rs`, `src/interactions_handler.rs`

---

## ADDED

### Requirement: `finish_with_upstream_error` Method

`RequestDiagnostics` exposes `finish_with_upstream_error` — a single method that records both a response dump and error stats for an upstream HTTP error. It replaces the two-call `response_dump` + `finish_with_error` pattern, eliminating the possibility of forgetting the response dump.

#### Scenario: Upstream HTTP error is recorded with response dump
- GIVEN an upstream returns a non-success HTTP status with an error body
- WHEN `finish_with_upstream_error(status, duration, size, upstream, dir, stream, error_body, headers)` is called
- THEN a response dump is recorded with `stage: "egress"`, `direction: "response"`, the error status, and the error body
- AND a stats event is recorded with the error status and the error body in `error` field
- AND deferred ingress/egress request dumps are flushed with `is_error: true`

#### Scenario: Internal errors continue using `finish_with_error`
- GIVEN an error originates internally (validation, session, stream infrastructure — no HTTP response body)
- WHEN `finish_with_error` is called directly (without `finish_with_upstream_error`)
- THEN a stats event is recorded with the error
- AND no response dump is recorded (there is no HTTP response to dump)
- AND the behavior is unchanged from before

---

## MODIFIED

### Requirement: Dump Event Coverage

The dump system records ingress request, egress request, and response events for every request. For upstream HTTP error responses, `finish_with_upstream_error` guarantees the error body is always captured in a response dump.

#### Scenario: Anthropic→OpenAI conversion error is dumped
- GIVEN Anthropic ingress is routed to an OpenAI upstream via protocol conversion
- AND `dump_mode` is `"error"` or `"all"`
- WHEN the OpenAI upstream returns an error status (4xx, 5xx)
- THEN `finish_with_upstream_error` is called, recording a response dump with `direction: "response"` and the upstream error body

#### Scenario: OpenAI→Anthropic conversion error is dumped
- GIVEN OpenAI ingress is routed to an Anthropic upstream via protocol conversion
- AND `dump_mode` is `"error"` or `"all"`
- WHEN the Anthropic upstream returns an error status (4xx, 5xx)
- THEN `finish_with_upstream_error` is called, recording a response dump with `direction: "response"` and the upstream error body

#### Scenario: Streaming conversion error is dumped
- GIVEN a streaming conversion request (either direction)
- AND `dump_mode` is `"error"` or `"all"`
- WHEN the upstream returns an error before streaming starts (non-success HTTP status)
- THEN `finish_with_upstream_error` is called, recording a response dump with the error body

#### Scenario: Passthrough error remains covered
- GIVEN a passthrough request (`openai→openai` or `anthropic→anthropic`)
- AND `dump_mode` is `"error"` or `"all"`
- WHEN the upstream returns an error
- THEN `finish_with_upstream_error` is called (replacing the old two-call pattern), behavior unchanged
