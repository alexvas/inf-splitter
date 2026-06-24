# Proposal: Fix 13 Interactions Protocol Correctness Bugs

**Change ID:** `fix-13-interactions-correctness-bugs`
**Created:** 2026-06-24
**Status:** Implementation Complete
**Completed:** 2026-06-24

---

## Problem Statement

Thirteen correctness bugs in interactions protocol handling cause:

- **Session corruption** — exact retry with empty `interaction_id` sends empty upstream request; client disconnect before `interaction.created` leaves unrecoverable pending session; session update failures silently ignored after successful upstream calls.
- **Content loss** — Anthropic `tool_result` blocks silently dropped during translation; OpenAI split-send streaming synthesis omits `tool_calls` from SSE chunks.
- **Split-send drift** — message count estimated from Content item proportions causes skip/duplicate on retry; chunk envelope uses hardcoded 36-byte `previous_interaction_id` so real longer IDs exceed `proxy_limit`.
- **Egress/header gaps** — `drop_fields` config ignored for interactions path; upstream response headers (rate-limit, trace) dropped from success responses; `x-request-id` (upstream-generated) not saved to session for next-request continuity; `x-claude-code-session-id` (client-provided) not forwarded as `X-Client-Request-Id` to OpenAI upstream.
- **Auth/config failures** — invalid API keys silently become empty auth headers; control sentinels trigger on single mention in user message, risking accidental session wipe if sentinel text appears in chat.
- **Token count wrapping** — fallback response casts `i64` to `u32` without clamping, producing bogus usage counts.

## Proposed Solution

Thirteen targeted fixes across `src/interactions_handler.rs`, `src/interactions.rs`, `src/control.rs`, and `src/auth.rs`:

1. **Empty interaction_id on exact retry** — when `start_index == incoming_count` and `prev_id` is `None` (empty interaction_id), return error instead of sending empty ContentList upstream.
2. **tool_result extraction** — extend `extract_anthropic_content` to read `tool_result` blocks and convert them to text content representing the tool result.
3. **Content-indexed progress** — track delivered Content items by index (not message proportion) in split-send session updates.
4. **tool_calls in OpenAI SSE synthesis** — extend `synthesize_openai_chunks` to emit `tool_calls` delta chunks when the response has `finish_reason=tool_calls`.
5. **Real ID in chunk envelope** — use the actual `previous_interaction_id` from the prior chunk (not `"x".repeat(36)`) for size estimation of subsequent chunks.
6. **Stream pending with empty ID recovery** — when stream task starts and `interaction_id` remains empty after client disconnect, mark session as not pending so startup recovery can clean it up, or set a flag that prevents blocking recovery.
7. **Session update error propagation** — log `tracing::error!` when `session_store.update` fails after successful upstream; return error to client so it can retry.
8. **drop_fields on interactions egress** — apply `drop_fields` from route config to `CreateModelInteractionParams` before serialization.
9. **Upstream header forwarding** — forward upstream response headers (rate-limit, request-id, trace) through interactions success responses via the same whitelist patterns used by passthrough paths.
10. **Clamp fallback token counts** — use `clamp_i64_to_u32` in `build_fallback_response` instead of `as u32`.
11. **API key validation** — validate `api_key` bytes as valid HTTP header values at config load time; reject invalid keys at startup.
12. **Header cross-mapping** — save upstream `x-request-id` response header to session state for next-request `previous_interaction_id` continuity; forward incoming `x-claude-code-session-id` as `X-Client-Request-Id` to OpenAI upstream.
13. **Control sentinel dedup** — require control constant to appear twice consecutively (`xyzxyz`) in the message text for `scan_control_messages` to trigger; single mention is treated as normal content. Prevents accidental session wipe when sentinel text leaks into chat.

## Scope

### In Scope
- All 13 findings listed above
- Unit tests for each fix (red-green TDD)
- Existing test suite must pass

### Out of Scope
- Architectural refactoring of interactions handler
- New protocol features
- Upstream Gemini API bug workarounds (except where proxy behavior is incorrect)
- Per-model `drop_fields` for interactions (flat list only, matching existing pattern)
- Authentication on `GET /interactions/v1/control-constants` — endpoint intentionally open; proxy access controlled at environment level

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions_handler.rs` | Yes | 10 fixes: exact retry, stream pending, session update, drop_fields, headers, split-send progress, chunk envelope, OpenAI tool_calls synthesis, fallback clamp, header cross-mapping |
| `interactions.rs` | Yes | 1 fix: tool_result extraction |
| `auth.rs` | Yes | 1 fix: API key validation |
| `control.rs` | Yes | 1 fix: sentinel dedup |
| `router.rs` | No | (control-constants endpoint unchanged) |
| `config.rs` | Maybe | API key validation at load time |

## Architecture Considerations

All fixes are localized to existing functions. No new modules, no new dependencies. Each fix follows existing patterns in the codebase.

## Success Criteria

- [ ] Exact retry with empty `interaction_id` returns error, not empty upstream request
- [ ] `extract_anthropic_content` reads `tool_result` blocks
- [ ] Split-send session progress uses Content index, not proportional estimation
- [ ] `synthesize_openai_chunks` emits `tool_calls` delta chunks
- [ ] Chunk envelope uses real `previous_interaction_id` length for size estimation
- [ ] Streaming disconnect with empty `interaction_id` does not leave unrecoverable pending session
- [ ] `session_store.update` failures after successful upstream are logged at error level and propagated
- [ ] `drop_fields` applied to interactions egress
- [ ] Upstream response headers forwarded through interactions success responses
- [ ] `build_fallback_response` uses `clamp_i64_to_u32` for token counts
- [ ] Invalid API keys rejected at config load time
- [ ] Upstream `x-request-id` saved to session state; `x-claude-code-session-id` forwarded as `X-Client-Request-Id` to OpenAI upstream
- [ ] Control sentinel triggers only on consecutive double appearance (`xyzxyz`), not single mention

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Single-sentinel mention in chat accidentally wipes sessions | Med | High | Require double consecutive appearance (`xyzxyz`); single mention is no-op |
| control-constants endpoint intentionally open — anyone with proxy access can read sentinels | Med | Med | Accepted risk; proxy access controlled at environment level. Explicitly out of scope |
| Header forwarding changes may leak unexpected headers | Low | Med | Apply same whitelist pattern as passthrough paths |
| API key validation at startup rejects previously-working configs | Low | Med | Log clear error message with the specific key that failed validation |
| tool_result extraction changes Content structure | Low | Low | Convert tool_result to text representation only (no structural change to Content enum) |

---

## Archive Information

**Archived:** 2026-06-24
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — 10 fixes: exact retry error, stream pending recovery, session update propagation, drop_fields application, upstream header forwarding, header cross-mapping, content-indexed split-send progress, real ID in chunk envelope, OpenAI SSE tool_calls synthesis, fallback token clamp
- `src/interactions.rs` — 2 fixes: tool_result extraction, `clamp_i64_to_u32` made public
- `src/control.rs` — 1 fix: double consecutive appearance for sentinel triggering
- `src/session.rs` — 1 fix: `update()` no longer returns error on `save_to_disk` failure
- `src/config.rs` — 1 fix: `api_key` validated via `HeaderValue::from_str`, new `InvalidApiKey` error variant
- `tests/e2e.rs` — Updated control message tests for double appearance

### Specs Updated
- `openspec/specs/protocol-conversion.md` — 12 new/modified requirements
- `openspec/specs/routing.md` — Control Constants endpoint intentionally unprotected
- `openspec/specs/config.md` — Secret Resolution validates header safety

### Quality Verification
- 384 tests passing
- `cargo fmt --check` clean
- `cargo clippy --locked` clean (no warnings)
