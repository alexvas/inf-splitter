# Implementation Tasks: Fix Split-Send Drops Tools

**Change ID:** `fix-split-send-drops-tools`

---

## Phase 1: Fix `handle_split_send` chunk loop

- [x] 1.1 Only pass `system_instruction` to the first chunk (when `current_prev.is_none()`)
- [x] 1.2 Set `tools` from `params.tools` on the first chunk
- [x] 1.3 Set `generation_config` from `params.generation_config` on the first chunk

**Quality Gate:**
- [x] `cargo check` passes

---

## Phase 2: Fix `send_split_system_instruction`

- [x] 2.1 Add `tools` and `generation_config` parameters to signature
- [x] 2.2 Pass them from `handle_split_send` call site
- [x] 2.3 Set on the first system-instruction chunk only

**Quality Gate:**
- [x] `cargo check` passes

---

## Phase 3: Integration & Polish

- [x] 3.1 Run full test suite (`cargo test --locked`)
- [x] 3.2 `cargo fmt --check`
- [x] 3.3 `cargo clippy --locked -- -D warnings`

**Quality Gate:**
- [x] All 342 tests pass
- [x] fmt and clippy clean

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Ready for `/openspec-archive`
