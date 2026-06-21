# Implementation Tasks: Fix Guard Deferred-Dump Edge Cases

**Change ID:** `fix-guard-deferred-dump-edge-cases`

---

## Phase 1: Write failing tests (RED)

- [ ] 1.1 Test: split-send with chunk 2 of 3 failing → prior chunk egress dumps have `is_error: false`
- [ ] 1.2 Test: passthrough success → ingress/egress dumps have status field populated
- [ ] 1.3 Test: request with no `messages` field → `messages_detail_ingress` absent from stats JSON
- [ ] 1.4 Test: split-send with 2 chunks → egress dumps have distinct capture-time timestamps
- [ ] 1.5 Test: split-send → response dump timestamp ≈ egress dump timestamp (same chunk paired)
- [ ] 1.6 Test: anthropic passthrough with `dump_mode: Off` → no egress body clone (structural test)

**Quality Gate:**
- [ ] 6 new tests FAIL (demonstrate the bugs)

---

## Phase 2: Fix StoredDump + deferred mechanism (GREEN — diagnostics.rs)

- [ ] 2.1 Extend `StoredDump` to `(DumpBody, Vec<(String, String)>, String, Option<u16>)` — add timestamp and optional status
- [ ] 2.2 Update `ingress_dump` to capture `ts_string()` at record time, `status: None`
- [ ] 2.3 Update `egress_dump` to capture `ts_string()` at record time, `status: None`
- [ ] 2.4 Add `egress_dump_with_status` or parameter for status (passthrough handlers call after knowing response status)
- [ ] 2.5 Update `flush_deferred_dumps` to use stored timestamps and status
- [ ] 2.6 Per-dump `is_error`: store `is_error: bool` per egress dump — fix split-send error path

**Quality Gate:**
- [ ] Tests 1.1, 1.2, 1.4 PASS

---

## Phase 3: Fix handler-level bugs (GREEN — openai.rs, anthropic.rs)

- [ ] 3.1 Fix `messages_detail_ingress` guard: change `unwrap_or(Value::Null)` to `if let Some`
- [ ] 3.2 Apply same fix to anthropic.rs passthrough
- [ ] 3.3 Gate anthropic passthrough egress clone + dump on `dump_enabled()`

**Quality Gate:**
- [ ] Tests 1.3, 1.6 PASS

---

## Phase 4: Cleanup

- [ ] 4.1 Remove dead `_route` parameter from `handle_stream_response` and its call site
- [ ] 4.2 Extract `header_pairs_from_map()` helper, use in `ingress_dump` and `egress_dump`
- [ ] 4.3 Extract `mark_finished(&self) -> bool` helper, use in `finish`, `finish_with_error`
- [ ] 4.4 Remove dead `disarm()` method
- [ ] 4.5 Remove unused `diagnostics_handle()` getter (check if DiagnosticStream still uses it — if so, keep)

**Quality Gate:**
- [ ] `cargo clippy` clean
- [ ] All existing tests pass

---

## Phase 5: Verify all tests pass

- [ ] 5.1 Run `cargo test` — all 279 existing + 6 new = 285+ tests pass
- [ ] 5.2 Run `cargo fmt --check`
- [ ] 5.3 Run `cargo clippy --locked -- -D warnings`

---

## Completion Checklist

- [ ] All phases complete
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy` clean
- [ ] `cargo test` passes (285+ tests)
- [ ] Spec delta committed
- [ ] Ready for `/openspec-archive`
