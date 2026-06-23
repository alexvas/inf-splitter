# Implementation Tasks: Diagnostics Guard Error-Path Invariant

**Change ID:** `guard-error-path-invariant`

---

## Phase 1: Fix Code

- [x] 1.1 `handle_split_send`: `pack_content_into_chunks` → `match` + `guard.finish_with_error`
- [x] 1.2 `send_split_system_instruction`: `split_text_for_limit` → `match` + `guard.finish_with_error`
- [x] 1.3 `send_and_translate`: `upstream.send().await?` → `match` + `guard.finish_with_error`
- [x] 1.4 `send_and_translate`: `upstream.bytes().await?` → `match` + `guard.finish_with_error`
- [x] 1.5 `send_and_translate`: `validate_upstream_body()?` → `match` + `guard.finish_with_error`
- [x] 1.6 `send_and_translate`: `serde_json::from_str().map_err()?` → `match` + `guard.finish_with_error`
- [x] 1.7 `send_and_translate`: `build_response_from_interaction().map_err()?` → `match` + `guard.finish_with_error`

**Quality Gate:**
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes
- [x] `cargo test --locked` passes (63/63)

---

## Phase 2: Spec Delta

- [x] 2.1 Add invariant to diagnostics.md spec ✓ 2026-06-23
- [x] 2.2 Add scenarios for all seven fixed paths ✓ 2026-06-23
- [x] 2.3 Review spec delta for completeness ✓ 2026-06-23

**Quality Gate:**
- [x] All new scenarios reference real code paths
- [x] Invariant is actionable for code review

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Ready for `/openspec-archive`
