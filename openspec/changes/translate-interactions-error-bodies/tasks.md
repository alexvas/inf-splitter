# Implementation Tasks: Translate Interactions Error Bodies

**Change ID:** `translate-interactions-error-bodies`

---

## Phase 1: Core Translation Function

- [x] 1.1 Add `translate_interactions_error_to_protocol` function in `src/lib.rs`
- [x] 1.2 Detect Gemini error shape: `error.message` (string, required), `error.code` (string, optional)
- [x] 1.3 Map `error.code` → Anthropic/OpenAI `error.type`, `error.message` → `error.message`
- [x] 1.4 Missing `error.code` defaults to `"api_error"`
- [x] 1.5 Non-Gemini bodies: pass through unchanged

**Quality Gate:**
- [x] 6 unit tests pass (Anthropic/OpenAI format, missing code default, non-JSON passthrough, non-Gemini JSON passthrough, preserves upstream code)

---

## Phase 2: Wire into Error Paths

- [x] 2.1 `send_and_translate` (~L523): translate before `apply_error_translation`
- [x] 2.2 `handle_split_send` (~L896): translate before `apply_error_translation`
- [x] 2.3 `send_split_system_instruction` first chunk (~L1083): translate before `apply_error_translation`
- [x] 2.4 `send_split_system_instruction` subsequent chunk (~L1149): translate before `apply_error_translation`

**Quality Gate:**
- [x] All 4 sites wired with `translate_interactions_error_to_protocol(&error_body, ingress)`

---

## Phase 3: Verify

- [x] 3.1 `cargo test --locked` — 306 tests pass
- [x] 3.2 `cargo fmt --check` — clean
- [x] 3.3 `cargo clippy --locked -- -D warnings` — clean

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [ ] Ready for `/openspec-archive`
