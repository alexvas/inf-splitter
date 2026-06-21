# Implementation Tasks: Add Auth Header Tests for Interactions

**Change ID:** `interactions-auth-header-tests`

---

## Phase 1: Unit tests for `build_interactions_headers` (RED→GREEN)

**RED:**
- [x] 1.1 `build_interactions_headers_sets_x_goog_api_key` — api_key → `x-goog-api-key` header ✓
- [x] 1.2 `build_interactions_headers_strips_client_auth_when_key_set` — client `Authorization` stripped when api_key is Some ✓
- [x] 1.3 `build_interactions_headers_forwards_client_auth_when_no_key` — client `Authorization` passed through when api_key is None ✓
- [x] 1.4 `build_interactions_headers_forwards_non_auth_headers` — `x-request-id` forwarded regardless of api_key ✓

**GREEN:** Tests pass immediately (code is already correct).

**Quality Gate:**
- [x] `cargo test --locked` — 4 new tests pass (184 unit total) ✓
- [x] `cargo clippy --locked` — clean ✓

---

## Phase 2: E2E tests for auth header behavior (RED→GREEN)

**RED:**
- [x] 2.1 `interactions_strips_client_auth_headers_when_api_key_set` — full Anthropic→Interactions dispatch with `api_key` set, verify upstream receives only `x-goog-api-key` ✓
- [x] 2.2 `interactions_sets_x_goog_api_key_from_config` — verify upstream receives `x-goog-api-key` matching config `api_key` ✓

**GREEN:** Tests pass immediately (code is already correct).

**Quality Gate:**
- [x] `cargo test --locked` — 2 new E2E tests pass (46 total) ✓
- [x] All existing tests continue to pass (184 unit + 28 integration + 46 E2E = 258 total) ✓
- [x] `cargo fmt` — applied ✓
- [x] `cargo clippy --locked` — clean ✓

---

## Completion Checklist

- [x] 4 unit tests added to `src/interactions_handler.rs` test module ✓
- [x] 2 E2E tests added to `tests/e2e.rs` ✓
- [x] All quality gates passed ✓
- [x] Ready for `/openspec-archive` ✓
