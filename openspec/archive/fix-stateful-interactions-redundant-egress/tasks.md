# Implementation Tasks: Fix Stateful Interactions Redundant Egress

**Change ID:** `fix-stateful-interactions-redundant-egress`

---

## Phase 1: Fix `compute_delta` (`src/session.rs`)

- [x] 1.1 Split `incoming <= delivered` into `<` (reset) and `==` (empty slice)
- [x] 1.2 Update `compute_delta_returns_same_when_no_new_messages` test: `(5,5)` → `(5,5)`
- [x] 1.3 Update `delta_no_new_messages_after_split` test: `(7,7)` → `(7,7)`

**Quality Gate:**
- [x] `cargo test -- session` passes
- [x] All delta-related test assertions are correct

---

## Phase 2: Fix `build_request_body` (`src/interactions.rs`)

- [x] 2.1 Extract `is_first = previous_interaction_id.is_none()`
- [x] 2.2 Guard `tools` with `is_first`
- [x] 2.3 Guard `tool_choice` with `is_first` (has_tool_choice = is_first && tool_choice.is_some())
- [x] 2.4 Guard `system_instruction` with `is_first`

**Quality Gate:**
- [x] `cargo test -- interactions` passes
- [x] Split-send path verified unaffected (`build_chunk_request` bypasses `build_request_body`)

---

## Phase 3: Integration & Polish

- [x] 3.1 Run full test suite (`cargo test --locked`)
- [x] 3.2 `cargo fmt --check`
- [x] 3.3 `cargo clippy --locked -- -D warnings`

**Quality Gate:**
- [x] All 251 unit + 28 e2e + 63 protocol conversion tests pass
- [x] fmt and clippy clean

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Ready for `/openspec-archive`
