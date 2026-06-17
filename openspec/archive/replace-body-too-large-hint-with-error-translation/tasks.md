# Implementation Tasks: Replace body_too_large_hint_statuses with error_translation

**Change ID:** `replace-body-too-large-hint-with-error-translation`

---

## Phase 1: Config & Data Model

- [x] 1.1 Add `ErrorTranslationRule` struct to `config.rs`
- [x] 1.2 Add `error_translation` field to `FileConfig` and `Config`
- [x] 1.3 Remove `body_too_large_hint_statuses` from `FileConfig` and `Config`
- [x] 1.4 Remove `default_body_too_large_hint_statuses()` helper
- [x] 1.5 Update `Config::from_file_config()` to parse `[[error_translation]]`
- [x] 1.6 Update `Config::from_model_routes()` test helper

**Quality Gate:**
- [x] `cargo check` passes
- [x] Config tests updated

---

## Phase 2: Core Logic (lib.rs)

- [x] 2.1 Remove `BODY_TOO_LARGE_HINT` constant
- [x] 2.2 Remove `append_size_hint()` function
- [x] 2.3 Add `apply_error_translation(status, body, rules) -> String`
- [x] 2.4 Simplify 413 middleware in `build_app()` — keep JSON error, drop hint
- [x] 2.5 Add unit tests for `apply_error_translation`

**Quality Gate:**
- [x] `cargo test --locked` passes
- [x] `cargo clippy --locked -- -D warnings` passes

---

## Phase 3: Handler Updates

- [x] 3.1 Remove `hint_statuses` from `OpenAiHandler::new()`, struct, and all 4 error paths
- [x] 3.2 Remove `hint_statuses` from `AnthropicHandler::new()`, struct, and all 4 error paths
- [x] 3.3 Pass `error_translation` rules (via `Arc<[ErrorTranslationRule]>`) to error paths
- [x] 3.4 Call `apply_error_translation()` instead of `append_size_hint()` in all error paths
- [x] 3.5 Update `relay_error_body()` in `anthropic.rs`

**Quality Gate:**
- [x] `cargo test --locked` passes
- [x] `cargo clippy --locked -- -D warnings` passes

---

## Phase 4: Documentation & Polish

- [x] 4.1 Update `README.md` global settings table
- [x] 4.2 Update `README.en.md` global settings table
- [x] 4.3 Update `README.zh.md` global settings table
- [x] 4.4 Update `config/inf-splitter.toml.example` with commented example
- [x] 4.5 Update CLAUDE.md config model summary

**Quality Gate:**
- [x] Pre-commit hook passes (heading count validation)
- [x] All READMEs in sync

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes
- [x] `cargo test --locked` passes
- [x] Documentation synced across all three READMEs
- [x] Ready for `/openspec-archive`
