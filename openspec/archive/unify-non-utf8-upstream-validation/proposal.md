# Proposal: Unify Non-UTF-8 Upstream Response Validation

**Change ID:** `unify-non-utf8-upstream-validation`
**Created:** 2026-06-21
**Status:** Draft

---

## Problem Statement

When an upstream returns binary (non-UTF-8) data with `content-type: application/json`, the downstream agent receives garbage that it cannot parse. The proxy must detect this and return a clean error.

Previously, each handler (`openai.rs`, `anthropic.rs`, `interactions_handler.rs`) had its own inline copy of the non-UTF-8 detection logic, and the interactions streaming path silently passed corrupted data via `String::from_utf8_lossy`.

## Proposed Solution

Extract a shared `validate_upstream_body` function in `lib.rs` that:
1. Checks bytes for valid UTF-8 via `dump_body_from_bytes`
2. Logs `tracing::warn!` on failure
3. Returns `ValidatedBody { text, dump }` on success, `AppError` on failure

Add non-UTF-8 protection to the interactions streaming path — reject binary chunks with an SSE error event instead of silently replacing invalid bytes with replacement characters.

## Scope

### In Scope
- `validate_upstream_body` helper in `lib.rs`
- Wire it into `openai.rs` and `anthropic.rs` non-streaming passthrough paths
- Wire it into 4 non-streaming interactions success paths
- Add non-UTF-8 detection to interactions streaming path
- Keep existing `tracing::warn!` and error message format identical

### Out of Scope
- Changes to error path handling (`unwrap_or_default` on non-2xx responses)
- Non-interactions streaming paths (already handled by openai/anthropic)
- Changes to existing test coverage

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `lib.rs` | Yes | Add `ValidatedBody`, `validate_upstream_body` |
| `openai.rs` | Yes | Use helper (1 site) |
| `anthropic.rs` | Yes | Use helper (1 site) |
| `interactions_handler.rs` | Yes | Use helper (4 non-streaming), add check (1 streaming) |

## Success Criteria

- [ ] All handlers use the same helper for non-UTF-8 detection
- [ ] Interactions streaming path aborts on binary chunks with SSE error event
- [ ] All 306 tests pass, fmt clean, clippy clean
