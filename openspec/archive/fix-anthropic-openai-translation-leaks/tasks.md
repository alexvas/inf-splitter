# Implementation Tasks: Strip Anthropic Fields from OpenAI Egress

**Change ID:** `fix-anthropic-openai-translation-leaks`

---

## Phase 1: Core Fix

- [x] 1.1 Add `sanitize_openai_egress` helper in `src/openai.rs`
- [x] 1.2 Import `ChatCompletionRequest` type
- [x] 1.3 Call `sanitize_openai_egress` after `cap_openai_max_tokens` in `handle_sync_manual`
- [x] 1.4 Call `sanitize_openai_egress` after `cap_openai_max_tokens` in `handle_stream_manual`
- [x] 1.5 Add `route.max_tokens` → `max_completion_tokens` limit transfer before stripping
- [x] 1.6 Add unit tests:
  - `sanitize_openai_egress_removes_context_management_from_extra`
  - `sanitize_openai_egress_removes_output_config_from_extra`
  - `sanitize_openai_egress_does_not_remove_unrelated_extra_fields`
  - `sanitize_openai_egress_nulls_max_tokens_when_completion_tokens_present`
  - `sanitize_openai_egress_preserves_max_tokens_when_completion_tokens_absent`
  - `sanitize_openai_egress_is_idempotent`

**Quality Gate:**
- [x] `cargo check` passes
- [x] `cargo test --locked` passes (266 tests)
- [x] `cargo clippy --locked -- -D warnings` clean
- [x] `cargo fmt --check` clean

---

## Phase 2: Verification

- [ ] 2.1 Manual end-to-end test: Claude CLI `ping` through inf-splitter with model `gpt-5.5`
- [ ] 2.2 Verify dump shows clean egress body (no `context_management`, `output_config`, `max_tokens`)
- [ ] 2.3 Verify `gpt-5.3-codex` model also works through the same section
- [ ] 2.4 Verify existing models (deepseek, gemini, etc.) continue to work

**Quality Gate:**
- [ ] End-to-end test passes
- [ ] No regressions in other models

---

## Completion Checklist

- [x] All Phase 1 tasks complete
- [ ] All Phase 2 tasks complete
- [ ] Ready for `/openspec-archive`
