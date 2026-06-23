# Proposal: Fix Split-Send Drops Tools and Generation Config

**Change ID:** `fix-split-send-drops-tools`
**Created:** 2026-06-23
**Status:** Draft

---

## Problem Statement

`build_chunk_request` builds a minimal `CreateModelInteractionParams` with only `model`, `input`, `stream`, `system_instruction`, and `previous_interaction_id`. It does **not** forward `tools` or `generation_config`.

When the first request in a session exceeds `proxy_limit` (e.g., 84KB of tools + 27KB of system_instruction + 14KB of input = ~126KB > 100KB limit), the request enters the split-send path. `handle_split_send` calls `build_chunk_request` for each chunk, which silently drops `tools` and `generation_config`. The upstream never sees the tool definitions, so the model cannot call tools.

The existing test `can_split_reports_per_tool_breakdown_from_dump` confirms that tools CAN be extracted from the dump format — the extraction is correct. The bug is purely in the split-send path: `build_chunk_request` + the callers don't forward these fields.

Additionally, `system_instruction` is passed to **every** chunk in the split path (line 809), not just the first one. This contradicts the `is_first` guard recently added to `build_request_body` — the split path bypasses it.

## Proposed Solution

### Fix 1: Forward `tools` and `generation_config` to the first chunk

In `handle_split_send`: after building the chunk request with `build_chunk_request`, set `tools` and `generation_config` from `params` on the **first chunk only** (when `current_prev` is `None`).

In `send_split_system_instruction`: same — first system-instruction chunk gets `tools` and `generation_config` from `params`.

### Fix 2: Only pass `system_instruction` to the first chunk

In `handle_split_send`: `system_instruction` should be `Some(...)` only for the first chunk; subsequent chunks get `None`. The interaction was already created with the system instruction.

### Fix 3 (optional, follow-up): Add `tools` and `generation_config` to `build_chunk_request` signature

Not strictly necessary — the fields can be set on the returned struct after construction. But adding them to the signature would make the API self-documenting.

## Scope

### In Scope
- Forward `tools` from `params` to the first chunk in `handle_split_send`
- Forward `generation_config` from `params` to the first chunk in `handle_split_send`
- Forward `tools` and `generation_config` to the first chunk in `send_split_system_instruction`
- Only pass `system_instruction` to the first chunk in `handle_split_send`
- Tests that verify tools survive the split-send path

### Out of Scope
- Refactoring `build_chunk_request` signature (can be done later)
- Fixing `send_split_system_instruction` to not pass system_instruction to non-first chunks (they already get individual parts, not the full system)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/interactions_handler.rs` | Yes | `handle_split_send`: set tools/gen_config on first chunk; `send_split_system_instruction`: same |
| `src/interactions.rs` | No | `build_chunk_request` unchanged — fields set after construction |
| `tests/` | Yes | New test: tools present in split-send egress |

## Architecture Considerations

- The split path intentionally constructs minimal requests per chunk. First-chunk-only fields (tools, gen_config, system_instruction) must be set conditionally — when `current_prev` is `None` (first chunk of the chain, which creates the interaction).
- `send_split_system_instruction` has its own first-chunk detection: `current_prev` starts as `None` and is set after the first response. Tools and gen_config follow the same pattern.

## Success Criteria

- [ ] Tools appear in the first split-send chunk egress dump
- [ ] Tools are absent from subsequent chunks (not re-sent)
- [ ] `generation_config` appears in the first split-send chunk
- [ ] `system_instruction` only on the first content chunk (not re-sent on follow-up chunks)
- [ ] All existing tests pass
- [ ] `cargo fmt --check`, `cargo clippy`, `cargo test` all pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Tools on first chunk inflate request beyond proxy_limit | Low | Medium | `can_split_under_limit` already checks that the envelope (including tools) fits under the limit; the first chunk will be within limit |
| First chunk with tools + system_instruction + content exceeds limit | Low | Medium | `can_split_under_limit` checks each single content element + envelope fits; greedy packing in `split_content_for_limit` respects the limit |


---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — `handle_split_send`: first-chunk gets `tools`, `generation_config`, `system_instruction`; `send_split_system_instruction`: first chunk gets `tools`, `generation_config`
- `tests/common/mod.rs` — `spawn_upstream_capture_all` helper

### Specs Updated
- `openspec/specs/protocol-conversion.md` — Proxy-Limit Split-Send Chunk Forwarding requirement
