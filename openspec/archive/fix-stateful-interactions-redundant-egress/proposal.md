# Proposal: Fix Stateful Interactions Redundant Egress

**Change ID:** `fix-stateful-interactions-redundant-egress`
**Created:** 2026-06-23
**Status:** Draft

---

## Problem Statement

The Gemini Interactions API is stateful — once an interaction is created, subsequent requests chain via `previous_interaction_id` and must only carry **new** content. The proxy was violating this contract in three ways:

1. **`system_instruction` (27KB) re-sent on every follow-up**: `build_request_body` unconditionally set `system_instruction` from the ingress, even when `previous_interaction_id` was present. The interaction already has the system instruction from creation — re-sending it is redundant and may confuse the model.

2. **`tools` and `tool_choice` (84KB of MCP tool definitions) re-sent on every follow-up**: Same pattern as `system_instruction` — tools are set once at interaction creation and cannot be changed mid-session. Re-sending them wastes bandwidth and processing.

3. **`compute_delta` re-sends messages when `incoming == delivered`**: The condition `incoming <= delivered` treated equal message counts as a "context reset," re-sending all messages from index 0. In a stateful protocol, equal counts mean *zero new messages* — the upstream already has this content.

The combination of these bugs produced an "echo loop" visible in diagnostics dumps: each egress request repeated previous content, causing the model to see duplicate messages and respond with variations of "I see we are stuck in a loop."

## Proposed Solution

Three targeted fixes in `build_request_body` and `compute_delta`:

### Fix 1: `compute_delta` — distinguish reset from no-new-messages

Split the `<=` condition into `<` (genuine reset) and `==` (no new messages → empty slice).

### Fix 2: `build_request_body` — first-interaction-only for tools and tool_choice

Introduce `is_first = previous_interaction_id.is_none()` guard. `tools` and `tool_choice` are only set when `is_first` is true.

### Fix 3: `build_request_body` — first-interaction-only for system_instruction

Same guard — `system_instruction` only set on the first interaction.

The split-send path (`handle_split_send` / `send_split_system_instruction`) is unaffected because it uses `build_chunk_request` which constructs `CreateModelInteractionParams` directly, bypassing `build_request_body`.

## Scope

### In Scope
- Fix `compute_delta` to not re-send when `incoming == delivered`
- Fix `build_request_body` to only set `tools`, `tool_choice`, `system_instruction` on first interaction
- Update existing unit tests to match new behavior
- Verify split-send path is unaffected (system_instruction chunks still chain correctly)

### Out of Scope
- Changing `temperature`/`max_output_tokens` behavior on follow-ups (may also be first-interaction-only in the API, but not causing observed bugs)
- Assistant-message filtering (model responses in conversation history are sent as input — a separate concern)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/session.rs` | Yes | Split `<=` into `<` and `==` in `compute_delta` |
| `src/interactions.rs` | Yes | Add `is_first` guard in `build_request_body` for tools, tool_choice, system_instruction |
| Split-send path | No | `build_chunk_request` is unaffected |

## Architecture Considerations

- The `is_first` boolean is derived from `previous_interaction_id.is_none()` — clean, single source of truth
- The split-send path (`build_chunk_request`) intentionally bypasses `build_request_body` and is unaffected by these changes
- The `compute_delta` change is a behavioral shift: `incoming == delivered` was previously treated as "re-send all" (conservative, wrong for stateful protocols); now it correctly produces an empty slice

## Success Criteria

- [x] `system_instruction` absent from egress dumps when `previous_interaction_id` is set
- [x] `tools` absent from egress dumps when `previous_interaction_id` is set
- [x] `compute_delta(N, N)` returns `(N, N)` (empty range) not `(0, N)` (re-send all)
- [x] Split-send with large system_instruction continues to chain chunks correctly
- [x] All existing tests pass
- [x] `cargo fmt --check`, `cargo clippy`, `cargo test` all pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| `compute_delta` returning empty slice causes empty-input interactions | Low | Low | `build_interactions_request_anthropic` produces empty `ContentList` — Gemini handles this gracefully (no-op interaction) |
| Split path broken by `is_first` guard | None | High | `build_chunk_request` bypasses `build_request_body` entirely — verified by code audit and existing split-send tests |
| Tools not sent on first request due to `is_first` logic error | None | Medium | `is_first` is `previous_interaction_id.is_none()` — correct for all first requests in a session |


---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/session.rs` — `compute_delta`: split `<=` into `<` and `==`
- `src/interactions.rs` — `build_request_body`: `is_first` guard for `tools`, `tool_choice`, `generation_config`, `system_instruction`

### Specs Updated
- `openspec/specs/protocol-conversion.md` — Anthropic→Interactions, OpenAI→Interactions: first-interaction-only fields, new delta scenarios
