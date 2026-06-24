# Implementation Tasks: Fix 13 Interactions Protocol Correctness Bugs

**Change ID:** `fix-13-interactions-correctness-bugs`
**Approach:** Red-Green (TDD)

---

## Phase 1: Session State & Recovery

- [x] 1.1 **RED** — Test: exact retry with empty `interaction_id` (session has `message_count=5, interaction_id=""`, client sends 5 messages) → handler returns error, not empty ContentList
- [x] 1.2 **GREEN** — When `start_index == incoming_count` and `prev_id` is `None` (empty interaction_id), return `AppError::Internal` instead of falling through to build empty request
- [x] 1.3 **RED** — Test: streaming session marked pending with `interaction_id=""`; client disconnects before `interaction.created` → session not left unrecoverable
- [x] 1.4 **GREEN** — In stream task error path (tx.send fails), when `interaction_id` is still empty, update session to `pending=false` so startup recovery can remove it
- [x] 1.5 **RED** — Test: `session_store.update` fails after successful upstream interaction → error logged and returned to client
- [x] 1.6 **GREEN** — Replace `let _ = self.session_store.update(...)` with proper error handling: `tracing::error!` on failure, return `AppError::Internal`

**Quality Gate:**
- [x] Unit tests pass for all 3 fixes
- [x] Existing session tests still pass

---

## Phase 2: Content Translation Correctness

- [x] 2.1 **RED** — Test: `extract_anthropic_content` with `[{"type":"tool_result","tool_use_id":"tu_1","content":"sunny"}]` → returns `Some(Content::TextContent)` with text "sunny"
- [x] 2.2 **GREEN** — Extend `extract_anthropic_content` to handle `tool_result` blocks — read `content` field (string or array of text blocks)
- [x] 2.3 **RED** — Test: `synthesize_openai_chunks` with response containing `tool_calls` → emits tool_calls delta chunk + finish_reason=tool_calls
- [x] 2.4 **GREEN** — Extend `synthesize_openai_chunks` to emit `tool_calls` delta chunks when `choice.message.tool_calls` is present
- [x] 2.5 **RED** — Test: `build_fallback_response` with `input_tokens = -1i64` → clamped to 0u32 with warning (not wrapped)
- [x] 2.6 **RED** — Test: `build_fallback_response` with `input_tokens = 5_000_000_000i64` → clamped to `u32::MAX` with warning
- [x] 2.7 **GREEN** — Replace `as u32` casts in `build_fallback_response` with `clamp_i64_to_u32` calls

**Quality Gate:**
- [x] Unit tests pass for all 3 fixes
- [x] Existing translation tests still pass

---

## Phase 3: Split-Send Correctness

- [x] 3.1 **RED** — Test: `pack_content_into_chunks` with real 50-char `previous_interaction_id` → all chunk bodies ≤ `proxy_limit`
- [x] 3.2 **GREEN** — In `handle_split_send`, pass the actual `last_id` (from prior chunk) as `previous_interaction_id` in `subsequent_envelope` instead of `"x".repeat(36)` after the first chunk completes. Rebuild `subsequent_envelope` with real ID length.
- [x] 3.3 **RED** — Test: 2 ingress messages produce 3 Content items → progress tracked by Content index, not proportional estimate
- [x] 3.4 **GREEN** — Track `delivered_content_count` by Content index (items sent so far), derive `message_count` via `min(delivered_content_count, total_message_count)` instead of proportional rounding

**Quality Gate:**
- [x] Unit tests pass for both fixes
- [x] Existing split-send tests still pass

---

## Phase 4: Egress & Header Handling

- [x] 4.1 **RED** — Test: interactions request built with `drop_fields = ["thinking"]` → `thinking` field absent from serialized body
- [x] 4.2 **GREEN** — Apply `route.resolve_drop_fields(&model)` to `CreateModelInteractionParams` before serialization in `send_and_translate` (non-streaming) and `handle_split_send`/chunk sender. Use `drop_fields_from_value` on serialized JSON.
- [x] 4.3 **RED** — Test: interactions upstream returns `x-ratelimit-*` headers → headers appear in client response
- [x] 4.4 **GREEN** — Forward upstream response headers through `ok_with_session_header` using `copy_response_headers`/`relay_response_headers` whitelist pattern
- [x] 4.5 **RED** — Test: upstream response with `x-request-id` → saved to session state; client request with `x-claude-code-session-id` → forwarded as `X-Client-Request-Id` to OpenAI upstream
- [x] 4.6 **GREEN** — Save upstream `x-request-id` response header to session state; forward incoming `x-claude-code-session-id` as `X-Client-Request-Id` in upstream requests

**Quality Gate:**
- [x] Unit tests pass for all 3 fixes
- [x] Existing egress/header tests still pass

---

## Phase 5: Auth & Security

- [x] 5.1 **RED** — Test: config with `api_key = "key\nwith\nnewlines"` → startup fails with clear error
- [x] 5.2 **GREEN** — Validate `api_key` bytes as valid HTTP header values at config resolution time (in `resolve_secret` or config validation). Reject keys containing bytes outside visible ASCII range or newlines.
- [x] 5.3 **RED** — Test: `scan_control_messages` with single sentinel mention → no-op (message passed through, no action)
- [x] 5.4 **RED** — Test: `scan_control_messages` with double consecutive sentinel (`xyzxyz`) → triggers control action
- [x] 5.5 **GREEN** — In `scan_control_messages`, require the constant to appear twice consecutively in the message text (`text.contains(&repeated)`) instead of single match (`text.contains(constant)`)

**Quality Gate:**
- [x] Unit tests pass for both fixes
- [x] Config validation tests pass

---

## Phase 6: Integration & Polish

- [x] 6.1 Run full test suite (`cargo test --locked`)
- [x] 6.2 Run clippy and fmt checks
- [x] 6.3 Verify no regressions in existing flows
- [x] 6.4 Update `openspec/specs/protocol-conversion.md` with new/modified requirements
- [x] 6.5 Update `openspec/specs/routing.md` with control-constants intentionally unprotected requirement
- [x] 6.6 Update `openspec/specs/config.md` with API key validation requirement

**Quality Gate:**
- [x] All tests pass
- [x] `cargo fmt --check` clean
- [x] `cargo clippy --locked` clean (no new warnings)
- [x] Specs updated
- [x] Ready for `/openspec-archive`
