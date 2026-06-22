# Implementation Tasks: Session ID Mapping Across Protocol Boundaries

**Change ID:** `add-session-id-mapping`

---

## Phase 1: Session ID Resolution

- [x] 1.1 RED: `resolve_session_id_uses_x_claude_code_session_id_when_x_request_id_absent` fails
- [x] 1.2 GREEN: add `x-claude-code-session-id` after `x-request-id` in `resolve_session_id()`
- [x] 1.3 RED: `resolve_session_id_prefers_x_request_id_over_x_claude_code_session_id` — header priority
- [x] 1.4 RED: `resolve_session_id_falls_back_to_body_request_id_when_no_headers` — body fallback
- [x] 1.5 RED: `resolve_session_id_uses_random_uuid_as_last_resort` — random UUID

**Quality Gate:**
- [x] 4/4 tests pass

---

## Phase 2: Egress Header Mapping (auth.rs)

- [x] 2.1 RED: `map_adds_x_request_id_from_x_claude_code_session_id` fails
- [x] 2.2 RED: `map_adds_x_claude_code_session_id_from_x_request_id` fails
- [x] 2.3 RED: `map_does_not_overwrite_when_both_headers_present` — identity
- [x] 2.4 GREEN: add bidirectional mapping in `forward_request_headers_map()`

**Quality Gate:**
- [x] 7/7 auth tests pass

---

## Phase 3: Response Header Mapping

- [x] 3.1 RED: `relay_response_headers_maps_x_request_id_to_x_claude_code_session_id` fails
- [x] 3.2 GREEN: add `x-claude-code-session-id` whitelist + mapping in `relay_response_headers()` (openai.rs)
- [x] 3.3 RED: `copy_response_headers_maps_x_claude_code_session_id_to_x_request_id` fails
- [x] 3.4 GREEN: add `x-claude-code-session-id`, `x-request-id` whitelist + mapping in `copy_response_headers()` (anthropic.rs)

**Quality Gate:**
- [x] 4/4 response tests pass (mapping direction verified by user review)

---

## Phase 4: Interactions Response Headers

- [x] 4.1 Add `session_header_name()` helper — maps `Protocol` → header name
- [x] 4.2 Wire session header into all non-streaming success paths (`send_and_translate`, `handle_split_send` chunks)
- [x] 4.3 Wire session header into streaming path via `sse_response_with_extra_header()`
- [x] 4.4 Wire session header into control message responses
- [x] 4.5 Wire session header into error response paths

**Quality Gate:**
- [x] 63 integration tests pass

---

## Phase 5: SSE Utility

- [x] 5.1 Add `sse_response_with_extra_header()` in `sse.rs`

---

## Completion Checklist

- [x] All phases complete
- [x] 247 unit + 63 integration = 0 failures
- [x] `cargo clippy --locked -- -D warnings` clean
- [x] `cargo fmt --check` clean
- [x] Ready for `/openspec-archive`
