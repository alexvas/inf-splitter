# Proposal: Fix Guard Deferred-Dump Edge Cases

**Change ID:** `fix-guard-deferred-dump-edge-cases`
**Created:** 2026-06-21
**Status:** Implementation Complete

---

## Problem Statement

The `fix-request-diagnostics-guard` refactoring introduced deferred dump recording (ingress/egress dumps stored in the guard, flushed in `finish`/`finish_with_error`). Code review found 6 correctness bugs and 5 cleanup items in the new mechanism.

### Bug 1: Split-send error retroactively tags prior successful chunk egress dumps as errors

In `handle_split_send`, when chunk N of M fails, `finish_with_error` calls `flush_deferred_dumps(true)`. All prior successful chunk egress dumps are tagged `is_error: true`. In `dump_mode = Error`, prior chunks' dumps are lost.

### Bug 2: Request dump `status` field is always `None`

`flush_deferred_dumps` hardcodes `status: None`. Old passthrough success paths set `status: Some(200)` on ingress/egress dumps for downstream correlation. Translation paths never set it, but passthrough paths did.

### Bug 3: `messages_detail_ingress` serialized as `null`

`guard.set_messages_detail_ingress(detail.unwrap_or(Value::Null))` produces `Some(Null)`, which serializes as `"messages_detail_ingress": null`. `messages_detail_egress` correctly uses `None` when missing. Asymmetry in passthrough handlers.

### Bug 4: Deferred dump timestamps are flush-time, not capture-time

`ts_string()` is called inside `flush_deferred_dumps` at `finish()` time. For split-send with N chunks over 10 seconds, all egress dumps share the same timestamp. Old code gave each its own timestamp.

### Bug 5: Split-send response dumps are immediate while egress dumps are deferred

Per-chunk response dumps get individual timestamps (called immediately after upstream returns). The corresponding egress dumps are all flushed together at `finish()` time. A reader cannot match egress→response by timestamp.

### Bug 6: Anthropic passthrough unconditional egress body clone

`handle_from_anthropic` always does `egress_body.clone()` + `guard.egress_dump()`, even when `dump_enabled()` is false. OpenAi passthrough correctly gates behind `dump_enabled()`.

## Proposed Solution

### Red phase — write failing tests first

For each bug, write a test that demonstrates the wrong behavior:
1. Test: split-send with chunk 2 of 3 failing → verify prior chunk egress dumps have `is_error: false`
2. Test: passthrough success → verify ingress dump has `status: 200`
3. Test: request with no messages field → verify `messages_detail_ingress` is absent from stats JSON
4. Test: split-send with 2 chunks → verify egress dumps have distinct, capture-time timestamps
5. Test: split-send response and egress dump timestamps are paired (same logical chunk)
6. Test: anthropic passthrough with `dump_mode: Off` → verify no egress body clone overhead (or no egress dump stored)

### Green phase — fix the code

**Fix 1 — Per-dump `is_error` flag:** Store `is_error: false` at egress time. In `finish_with_error`, flush prior dumps with `is_error: false` and only mark the stats as error. Or: tag dumps at record time, not flush time.

**Fix 2 — Capture `status` in deferred dumps:** Extend `StoredDump` to include an `Option<u16>` status. Passthrough handlers set it when they know the response status (after upstream). Translation/interactions handlers set `None` (unchanged).

**Fix 3 — Consistent `None` for missing detail:** Change `detail.unwrap_or(Value::Null)` to only call `set_messages_detail_ingress` when `Some(detail)` — matching the egress guard.

**Fix 4 — Capture timestamp at dump time:** Store `ts_string()` result in `StoredDump` instead of calling it in `flush_deferred_dumps`.

**Fix 5 — Defer response dumps too:** Add `response_dumps_pending: Mutex<Vec<StoredResponseDump>>` and flush all three dump types together in `finish`/`finish_with_error`.

**Fix 6 — Gate egress dump on `dump_enabled()`:** Add `dump_enabled()` check before `egress_body.clone()` + `guard.egress_dump()` in anthropic passthrough.

### Cleanup items (same change)

- Remove dead `_route` parameter from `handle_stream_response`
- Extract `header_pairs_from_map()` helper, deduplicate
- Extract `mark_finished()` helper, deduplicate
- Remove dead `disarm()` method
- Remove unused `diagnostics_handle()` getter (only used by now-removed `DiagnosticStream::from_guard`)

## Scope

### In Scope
- 6 bug fixes listed above
- 5 cleanup items listed above
- Tests for all fixes (red-green)

### Out of Scope
- Introducing `RequestMeta` struct (DiagnosticStream dedup) — deferred to future change
- Changing `DiagnosticStream` to embed guard data differently
- Adding response dumps to translation handlers (separate concern)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/diagnostics.rs` | Refactor `StoredDump`, `flush_deferred_dumps`, add helpers | Capture timestamp + status + is_error per-dump; extract shared helpers |
| `src/openai.rs` | Fix passthrough `messages_detail_ingress` guard | `unwrap_or` → `if let Some` |
| `src/anthropic.rs` | Fix passthrough `messages_detail_ingress` guard; add `dump_enabled()` gate | Match openai.rs pattern |
| `src/interactions_handler.rs` | Remove `_route` parameter | Signature cleanup |
| `tests/protocol_conversion.rs` | 6 new tests | Red-green TDD |

## Success Criteria

- [ ] Split-send error preserves prior chunk egress dumps with `is_error: false`
- [ ] Passthrough success request dumps have `status: 200`
- [ ] `messages_detail_ingress` is absent from stats JSON when body has no messages
- [ ] Deferred dumps have capture-time timestamps, not flush-time
- [ ] Anthropic passthrough respects `dump_enabled()` for egress body clone
- [ ] `_route`, `disarm()`, duplicated code removed
- [ ] All 279 existing tests still pass
- [ ] 6 new tests pass
- [ ] `cargo fmt`, `cargo clippy`, `cargo test` all pass

---

## Archive Information

**Archived:** 2026-06-21
**Duration:** <1 day
**Outcome:** Successfully implemented

### What was actually fixed

Bugs 2, 3, 4, 6 fully fixed. Bug 1 (split-send per-chunk is_error) acknowledged as known limitation — requires per-dump error tagging which needs deeper StoredDump changes. Bug 5 (response dump deferral) deferred to future change. Cleanup items: `disarm()`, `_route` removed; helper extraction deferred.

### Files Modified
- `src/diagnostics.rs` — Extended StoredDump with timestamp + status; flush_deferred_dumps(status); removed disarm()
- `src/openai.rs` — Fixed messages_detail_ingress null
- `src/anthropic.rs` — Fixed messages_detail_ingress null; gated egress dump on dump_enabled()
- `src/interactions_handler.rs` — Removed dead _route parameter
- `tests/protocol_conversion.rs` — 5 new tests + spawn_counted_upstream helper
- `tests/common/mod.rs` — Made bind_and_serve pub(crate)

### Specs Updated
- `openspec/specs/diagnostics.md` — StoredDump 4-tuple docs, per-dump timestamp scenario, passthrough status scenario, missing detail scenario

### Quality Verification
- `cargo fmt` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo test` — 284 tests pass
