# Delta: Request Routing

**Change ID:** `replace-body-too-large-hint-with-error-translation`
**Affects:** `src/lib.rs`, `src/openai.rs`, `src/anthropic.rs`

---

## ADDED

### Requirement: Upstream Error Body Translation

When an upstream returns a non-success HTTP status with a non-streaming (`text/event-stream`) response, the error body is checked against the configured `[[error_translation]]` rules. On match, the body is replaced with the rule's `egress` string. On no match, the body passes through unchanged.

Translation applies to all four routing directions (openai→openai, openai→anthropic, anthropic→anthropic, anthropic→openai) and both streaming and non-streaming error paths.

The `apply_error_translation(status, body, rules) -> String` function in `lib.rs` implements the matching logic — iterating rules in order, returning the translated body on first match, or the original body if no rule matches.

#### Scenario: Upstream 413 error translated
- GIVEN `[[error_translation]]` has `{status = 413, egress = "body too large"}`
- WHEN upstream returns 413 with any body
- THEN the client receives 413 with body `"body too large"`

#### Scenario: Upstream 502 error translated
- GIVEN `[[error_translation]]` has `{status = 502, egress = "BODY TOO LARGE"}`
- WHEN upstream returns 502
- THEN the client receives 502 with body `"BODY TOO LARGE"`

#### Scenario: Upstream error passes through
- GIVEN no matching translation rule for status 500
- WHEN upstream returns 500 with body `"internal error"`
- THEN the client receives 500 with body `"internal error"`

#### Scenario: Substring match required
- GIVEN `{status = 413, ingress = "vague", egress = "translated"}`
- WHEN upstream returns 413 with body `"some other error"`
- THEN body passes through unchanged (substring does not match)

---

## MODIFIED

### Requirement: Body Size Limit

The proxy enforces `max_request_body` via `tower-http::limit::RequestBodyLimitLayer`. On 413 errors, a JSON error is returned:

```json
{"type":"error","error":{"type":"invalid_request_error","message":"Request body exceeds limit."}}
```

The hint about reducing context size is removed. (Previously this was controlled by `body_too_large_hint_statuses`; the hint string is now removed.)

#### Scenario: Body exceeds limit
- GIVEN `max_request_body = "1m"`
- WHEN a request body exceeds 1 MiB
- THEN the proxy returns 413 with a JSON error (no hint suffix)

---

## REMOVED

### Requirement: Body Too Large Hint

The `append_size_hint()` function, `BODY_TOO_LARGE_HINT` constant, and the `body_too_large_hint_statuses` config field are removed. The 413 middleware no longer appends "Try reducing context size or splitting into smaller requests." to the error message.
