# Proposal: Deduplicate Interactions Handler

**Change ID:** `deduplicate-interactions-handler`
**Created:** 2026-06-22
**Status:** Implemented

---

## Problem Statement

`interactions_handler.rs` contained three duplicated code patterns:

1. **Control action handling** — 70-line block repeated identically in `handle_from_anthropic` and `handle_from_openai`, differing only in the response header name (`x-claude-code-session-id` vs `x-request-id`)
2. **Fallback response from last interaction** — 65-line block repeated in `handle_split_send` and `send_split_system_instruction`, building a `ChatCompletionResponse` or `MessageResponse` from usage stats
3. **Session header on 200 OK JSON response** — 6-line block repeated 5 times across `handle_control_action` (×2), `send_and_translate`, `handle_split_send`, `send_split_system_instruction`

## Proposed Solution

Extract three helpers:

| Helper | Type | Replaces |
|--------|------|----------|
| `handle_control_action` | Method on `InteractionsHandler` | 2× ~70 lines |
| `build_fallback_response` | Standalone function | 2× ~65 lines |
| `ok_with_session_header` | Method on `InteractionsHandler` | 5× ~6 lines |

## Scope

### In Scope
- `src/interactions_handler.rs` — extract helpers, replace call sites

### Out of Scope
- Other files, behavior changes

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/interactions_handler.rs` | Yes | ~150 lines removed, 3 helpers added |

## Success Criteria

- [x] `handle_control_action` replaces both control action blocks
- [x] `build_fallback_response` replaces both fallback response blocks
- [x] `ok_with_session_header` replaces all 5 session-header-on-OK-response sites
- [x] `cargo test` — 306 tests pass
- [x] `cargo clippy --locked -- -D warnings` — clean
- [x] `cargo fmt --check` — clean

---

## Archive Information

**Archived:** 2026-06-22
**Duration:** < 1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — extracted `handle_control_action`, `build_fallback_response`, `ok_with_session_header`

### Specs Updated
- `openspec/specs/routing.md` — Control Action Helper, Fallback Response Builder, OK Response with Session Header
