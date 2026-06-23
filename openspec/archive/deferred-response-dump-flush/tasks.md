# Implementation Tasks: Deferred Response Dump Flush

**Change ID:** `deferred-response-dump-flush`

---

## Phase 1: Defer Response Dumps

- [x] 1.1 Add `response_dump_pending: Mutex<Option<StoredDump>>` to `RequestDiagnostics`
- [x] 1.2 Change `response_dump` to store in pending instead of calling `record_response_dump`
- [x] 1.3 Change `response_dump_streaming` to store in pending
- [x] 1.4 Flush response_dump_pending in `flush_deferred_dumps` (both `finish` and Drop paths)

**Quality Gate:**
- [x] `cargo check` passes
- [x] `cargo clippy` passes

---

## Phase 2: Disk Flush

- [x] 2.1 Add `sync_data()` to `RotatingWriter::flush()` after `BufWriter::flush()`

**Quality Gate:**
- [x] `cargo check` passes
- [x] `cargo clippy` passes

---

## Phase 3: Test Stabilization

- [x] 3.1 In `poll_diagnostics_file`: after predicate satisfied, sleep 20ms, re-read, check size unchanged before returning

**Quality Gate:**
- [x] Flaky test passes 10/10
- [x] Full test suite passes

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] `cargo fmt --check` clean
- [x] `cargo clippy --locked -- -D warnings` clean
- [x] `cargo test --locked` all passing (341 tests)
