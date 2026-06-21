# Implementation Tasks: Unify Non-UTF-8 Upstream Validation

**Change ID:** `unify-non-utf8-upstream-validation`

---

## Phase 1: Shared Helper

- [x] 1.1 Add `ValidatedBody` struct and `validate_upstream_body` function in `src/lib.rs`
- [x] 1.2 Wire into `openai.rs` passthrough path (1 site)
- [x] 1.3 Wire into `anthropic.rs` passthrough path (1 site)

**Quality Gate:**
- [x] Compiles, all existing tests pass

---

## Phase 2: Interactions Handler

- [x] 2.1 Wire helper into 4 non-streaming success paths (`send_and_translate`, `handle_split_send`, `send_split_system_instruction` ×2)
- [x] 2.2 Add non-UTF-8 detection to streaming path — replace `from_utf8_lossy` with `from_utf8` check, send SSE error event, abort stream

**Quality Gate:**
- [x] All 306 tests pass, fmt clean, clippy clean

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [ ] Ready for `/openspec-archive`
