# Delta: Diagnostics

**Change ID:** `fix-interactions-protocol-correctness`
**Affects:** `src/interactions_handler.rs`

---

## MODIFIED

### Requirement: Stats Event Format

**Change:** Interactions split-send error paths must pass the actual `stream` flag to `finish_with_upstream_error`, not hardcoded `false`.

#### Scenario: Streaming flag correct in split-send errors
- GIVEN client requested `stream: true` and a split-send chunk gets upstream error
- WHEN stats event is recorded
- THEN `"streaming": true` is written to the stats line
- INSTEAD OF: `"streaming": false` (hardcoded)

---

## ADDED

### Requirement: Interactions Response Header Diagnostics Whitelist

Interactions diagnostics must filter upstream response headers through `is_interactions_response_header_whitelisted` before recording in dumps, matching the pattern used by passthrough response handlers.

#### Scenario: Whitelisted headers captured
- GIVEN interactions upstream returns `x-request-id: abc` and `x-ratelimit-remaining: 100`
- WHEN diagnostics records response headers
- THEN both headers are included in the dump

#### Scenario: Non-whitelisted headers excluded
- GIVEN interactions upstream returns `Set-Cookie: session=xyz` header
- WHEN diagnostics records response headers
- THEN `Set-Cookie` is not present in the dump

#### Scenario: Non-streaming paths also filter
- GIVEN interactions non-streaming response has `response_headers_to_pairs` result
- WHEN `guard.response_dump` or `guard.finish_with_upstream_error` is called
- THEN headers are filtered through the whitelist
