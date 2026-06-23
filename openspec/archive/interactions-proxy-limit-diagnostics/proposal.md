# Proposal: Interactions Proxy-Limit Diagnostics

**Change ID:** `interactions-proxy-limit-diagnostics`
**Created:** 2026-06-23
**Status:** Archived
**Completed:** 2026-06-23

---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — early guard creation, threading to sub-functions, error-path diagnostics
- `src/interactions.rs` — `format_bytes()`, `tool_size_breakdown()`, per-field envelope breakdown, lazy closure

### Files Added
- `tests/data/single_request.dump` — production dump fixture
- `tests/data/tools_from_dump.json` — extracted 105 tools

### Specs Updated
- `openspec/specs/diagnostics.md` — added envelope breakdown, per-tool breakdown, lazy computation, format_bytes requirements

---

## Problem Statement

When `can_split_under_limit` fails in the interactions handler (`handle_from_anthropic` / `handle_from_openai`), no diagnostics (dump or stats) are recorded. The error returns to the client as a 400, but neither the ingress dump nor a stats entry appear in the diagnostic output files — even when `stats_mode = "all"` and `dump_mode = "all"`.

Root cause: `RequestDiagnostics` guard is created too late — in `send_and_translate` and `handle_split_send` — but the `can_split_under_limit` check runs earlier in `handle_from_anthropic` / `handle_from_openai`, before either sub-function is called.

Additionally, the error message for envelope-too-large gives no breakdown of *which* non-splittable fields consume the space, making it hard to debug without manual JSON inspection. And when `tools` are the culprit, there's no per-tool size breakdown to identify which specific tool definitions need trimming.

## Proposed Solution

1. **Move `RequestDiagnostics` guard creation up** to the handler entry points (`handle_from_anthropic`, `handle_from_openai`), immediately after model extraction. Record `ingress_dump` on the guard and thread it through to `send_and_translate`, `handle_split_send`, and `handle_control_action` (which previously had no diagnostics at all).

2. **Call `guard.finish_with_error(400, ...)` on `can_split_under_limit` failure**, capturing the full error details in diag while returning a short message to the client.

3. **Add per-field envelope size breakdown** in `can_split_under_limit` error messages: model, stream, generation_config, tools, previous_interaction_id — each with byte count and human-readable size (KiB/MiB).

4. **Add `tool_size_breakdown()`** — for each `Tool::Function`, show name, total serialized size, description size, and parameters schema size in human-readable units. Appended to all three `can_split_under_limit` error paths so it appears in diag's `error` field.

5. **Add test with real-world tool data** extracted from a production Claude Code dump (105 tools, 160 KiB). The test verifies the full error output format.

## Scope

### In Scope
- Diagnostics (dump + stats) for `can_split_under_limit` 400 errors
- Diagnostics for `handle_control_action` success paths
- Per-field envelope size breakdown in error
- Per-tool size breakdown (name, total, description, parameters)
- Lazy computation: tool breakdown only computed when limit actually exceeded
- Test fixture from real-world dump data

### Out of Scope
- Splitting tools across interactions (semantically invalid)
- Per-field breakdown for "single content element too large" or "system instruction word too large" paths beyond tool info
- Changing `proxy_limit` defaults or semantics

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions.rs` | Yes | `can_split_under_limit` extended; new `format_bytes`, `tool_size_breakdown` helpers |
| `interactions_handler.rs` | Yes | Guard created early; threaded to sub-functions; error-path diagnostics |
| `diagnostics.rs` | No | No schema changes — uses existing `finish_with_error` |
| Tests | Yes | New test `can_split_reports_per_tool_breakdown_from_dump`; fixture data added |

## Architecture Considerations

Follows existing patterns:
- Guard threading matches `send_split_system_instruction` which already accepts `guard: RequestDiagnostics`
- Error messages use the `StatsEvent.error` field (already a free-form string) — no schema change needed
- Lazy closure (`let tool_info = || ...`) avoids unnecessary serialization on the happy path

## Success Criteria

- [ ] `can_split_under_limit` 400 errors produce an ingress dump in `dump_output`
- [ ] `can_split_under_limit` 400 errors produce a stats entry with `status: 400` and the full error in `diag_output`
- [ ] Envelope error message lists each field with byte count and human-readable size
- [ ] Tool breakdown includes name, total size, description size, parameters size for each `Tool::Function`
- [ ] Client receives short message: "Request cannot be split under proxy limit (see diagnostics for details)"
- [ ] All existing tests pass; new test covers the full output format
