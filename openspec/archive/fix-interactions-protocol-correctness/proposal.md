# Proposal: Fix Interactions Protocol Correctness Bugs

**Change ID:** `fix-interactions-protocol-correctness`
**Created:** 2026-06-24
**Status:** Implementation Complete
**Completed:** 2026-06-24

---

## Problem Statement

Thirteen correctness bugs in interactions protocol handling cause:

- **Auth failures** — lifecycle operations (cancel/delete/get) strip query parameters from `endpoint_interactions`, dropping `?key=ABC` and getting 401/403.
- **Token contract violations** — `route.max_tokens` overrides client's lower limit instead of capping it; OpenAI `max_completion_tokens` ignored entirely; u64 usage values silently wrap in release builds.
- **Session corruption** — context reset keeps stale `previous_interaction_id`; stream disconnect leaves session `pending=true` with empty `interaction_id`; exact retries produce empty upstream requests; message count drift from Content-vs-message mismatch.
- **SSE protocol violations** — duplicate `ContentBlockStop` events from `InteractionCompletedEvent`.
- **Oversized upstream requests** — system-instruction splitting ignores envelope overhead, sending requests up to 60KB over `proxy_limit`.
- **Silent error swallowing** — system-instruction chunk deserialization failures pass silently, causing corrupted state chains.
- **Unrecoverable failures** — session marked complete before final translation succeeds, making retries impossible.
- **System message loss** — `extract_openai_system` only checks first position, dropping system messages elsewhere.

## Proposed Solution

Ten targeted fixes in `src/interactions_handler.rs`, `src/interactions.rs`, and `src/session.rs`:

1. **Preserve query params** in `build_interaction_url` — parse endpoint URL, reattach query string to lifecycle operation paths
2. **Cap token limits** — use `min(client_max_tokens, route_max_tokens)` instead of `route_max_tokens.or(client_max_tokens)`
3. **Fix context reset** — clear `previous_interaction_id` when `start_index == 0` (both Anthropic and OpenAI paths)
4. **Remove duplicate ContentBlockStop** — skip emitting `content_block_stop` in `InteractionCompletedEvent` handler when the last step already stopped that block
5. **Fix envelope measurement** — use full serialized body (envelope + content) for system-instruction split check
6. **Handle deserialization errors** — replace `if let Ok` with proper `match`/`?` error propagation in `send_split_system_instruction`
7. **Fix streaming pending state** — set `interaction_id` from `InteractionCreatedEvent` immediately on the session, so early-disconnect recovery works
8. **Handle exact retries** — when `compute_delta` returns empty delta and `previous_interaction_id` is `Some`, replay the last interaction's response via `GET /v1beta/interactions/{id}` instead of sending empty input
9. **Fix message count tracking** — count ingress messages (after control stripping), not Content items, for session `message_count`
10. **Fix completion order** — translate final response BEFORE marking session `pending = false`
11. **Read `max_completion_tokens`** in OpenAI→Interactions path, falling back to `max_tokens`
12. **Scan all positions** for system message in `extract_openai_system`, not just `messages.first()`
13. **Clamp u64→u32** with saturating conversion and warning, instead of silent wrapping `as u32`

## Scope

### In Scope
- All 13 findings listed above
- Unit tests for each fix
- Existing test suite must pass

### Out of Scope
- Architectural refactoring of interactions handler
- New protocol features
- Upstream Gemini API bug workarounds (except where proxy behavior is incorrect)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions_handler.rs` | Yes | 10 fixes: URL, context reset, duplicate stop, envelope, error handling, pending state, exact retries, message count, completion order, max_completion_tokens |
| `interactions.rs` | Yes | 3 fixes: max_tokens cap, u64→u32 clamp, extract_openai_system |
| `session.rs` | Yes | compute_delta behavior + empty delta handling |

## Architecture Considerations

All fixes are localized to existing functions. No new modules, no new dependencies. Each fix follows existing patterns in the codebase.

## Success Criteria

- [ ] `build_interaction_url` preserves query parameters from `endpoint_interactions`
- [ ] `route.max_tokens` acts as a cap, not an override — `min(client, route)` semantics
- [ ] Context reset (`start_index == 0`) clears `previous_interaction_id`
- [ ] `InteractionCompletedEvent` does not emit duplicate `ContentBlockStop`
- [ ] System-instruction splitting measures full serialized body against `proxy_limit`
- [ ] `send_split_system_instruction` propagates deserialization errors
- [ ] Streaming disconnect recovery finds valid `interaction_id` from `InteractionCreatedEvent`
- [ ] Exact retries (empty delta + `previous_interaction_id`) fetch existing interaction instead of sending empty input
- [ ] Session `message_count` tracks ingress message count, not Content item count
- [ ] Session marked complete only AFTER final response translation succeeds
- [ ] `max_completion_tokens` read in OpenAI path (fallback to `max_tokens`)
- [ ] `extract_openai_system` scans all message positions for system role
- [ ] u64 token counts use saturating conversion to u32 with `tracing::warn!`

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Exact retry GET change alters session behavior | Low | Med | Guard behind `previous_interaction_id.is_some() && delta_empty` — only fires on genuine retries |
| Message count fix may change existing sessions | Med | Med | Session TOML format unchanged; only new updates affected. Pending sessions recovered on startup |
| max_tokens cap change surprises users relying on override behavior | Low | Low | The override was always a bug — documented as cap in spec |

---

## Archive Information

**Archived:** 2026-06-24
**Duration:** 0 days (same-day)
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — 10 fixes
- `src/interactions.rs` — 3 fixes

### Specs Updated
- `openspec/specs/protocol-conversion.md` — 7 new requirements added, 4 existing requirements modified
