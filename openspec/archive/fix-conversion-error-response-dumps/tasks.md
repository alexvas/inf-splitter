# Implementation Tasks: Fix Conversion Error Response Dumps

**Change ID:** `fix-conversion-error-response-dumps`

---

## Phase 1: `finish_with_upstream_error` method

- [x] 1.1 RED — write test for `finish_with_upstream_error` in `src/diagnostics.rs` tests: verifies that the method records a response dump (via `response_dump_pending` being consumed) and marks the guard as finished with correct error stats ✓ 2026-06-23
- [x] 1.2 GREEN — add `finish_with_upstream_error` method to `RequestDiagnostics`; run `cargo test -p inf-splitter -- diagnostics` — test passes ✓ 2026-06-23

**Quality Gate:** PASSED
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes
- [x] `cargo test --locked` passes

---

## Phase 2: Fix `openai.rs` call sites (3 sites)

- [x] 2.1 RED — bug confirmed via real dump file (`dump-codex-1.ndjson`): conversion error paths produce no response dump entries ✓ 2026-06-23
- [x] 2.2 GREEN — replace 3 call sites (`handle_from_openai`, `handle_sync_manual`, `handle_stream_manual`) with `finish_with_upstream_error`; header collection added to conversion paths for complete dump context ✓ 2026-06-23

**Quality Gate:** PASSED
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes
- [x] `cargo test --locked` passes

---

## Phase 3: Fix `anthropic.rs` call sites (3 sites)

- [x] 3.1 RED — bug confirmed: same root cause as Phase 2, `handle_from_openai` and `handle_from_openai_stream` missing `response_dump` in error paths ✓ 2026-06-23
- [x] 3.2 GREEN — replace 3 call sites (`handle_from_anthropic`, `handle_from_openai`, `handle_from_openai_stream`) with `finish_with_upstream_error`; header collection added to conversion paths ✓ 2026-06-23

**Quality Gate:** PASSED
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes
- [x] `cargo test --locked` passes

---

## Phase 4: Replace `interactions_handler.rs` call sites (4 sites)

- [x] 4.1 REFACTOR — replace 4 `response_dump` + `finish_with_error` pairs with `finish_with_upstream_error` (these were already correct, behavior preserved, verified by existing tests) ✓ 2026-06-23
- [x] 4.2 Verify no other `response_dump` + `finish_with_error` pairs remain in the codebase — only success-path `response_dump` (status 200) and the method body itself remain ✓ 2026-06-23

**Quality Gate:** PASSED
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes
- [x] `cargo test --locked` passes

---

## Completion Checklist

- [x] `finish_with_upstream_error` method added with 2 unit tests (finishes guard, idempotent)
- [x] All 10 upstream HTTP error call sites use the new method
- [x] All 8 internal error call sites remain on `finish_with_error` (unchanged)
- [x] Characterization tests removed after serving their purpose
- [x] Full CI checks pass (`fmt`, `clippy`, `test` — 351 tests)
- [x] Ready for `/openspec-archive`
