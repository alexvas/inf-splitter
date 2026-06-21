# Proposal: Add Auth Header Tests for Interactions

**Change ID:** `interactions-auth-header-tests`
**Created:** 2026-06-20
**Status:** Implementation Complete
**Completed:** 2026-06-20

---

## Problem Statement

`build_interactions_headers` was recently fixed to strip client auth headers when `api_key` is set (preventing Gemini `OVERLOADED_CREDENTIALS` errors). However, this function has **zero direct test coverage** — neither unit tests nor E2E tests verify the new behavior.

Existing tests that touch auth headers:

| Test | File | Covers |
|------|------|--------|
| `forward_request_headers_applies_auth_override` | `src/auth.rs` | `forward_request_headers` with `api_key=Some` |
| `forward_request_headers_forwards_incoming_auth_when_no_override` | `src/auth.rs` | `forward_request_headers` with `api_key=None` |
| `interactions_forwards_client_headers_to_upstream` | `tests/e2e.rs:1400` | Non-auth client headers forwarded WITHOUT `api_key` |

**None of these test `build_interactions_headers`** — the function that was actually changed. The archived tasks (Phase 8, tasks 8.1-8.4) claimed auth tests were written, but they tested `forward_request_headers` indirectly through the old code path that no longer exists.

### Specific gaps:
1. No test that `x-goog-api-key` is added when `api_key` is configured
2. No test that client `Authorization` / `x-api-key` headers are stripped when `api_key` is set
3. No test that client auth headers pass through when `api_key` is NOT set
4. No E2E test verifying these behaviors on the full request path

## Proposed Solution

### Unit tests (in `src/interactions_handler.rs` test module):

**`build_interactions_headers_sets_x_goog_api_key`**
- GIVEN `api_key = Some("gemini-key-123")`
- WHEN `build_interactions_headers` is called with client headers
- THEN `x-goog-api-key: gemini-key-123` header is set

**`build_interactions_headers_strips_client_auth_when_key_set`**
- GIVEN `api_key = Some("gemini-key-123")` and client sends `Authorization: Bearer sk-ant-...`
- WHEN `build_interactions_headers` is called
- THEN `Authorization` header is NOT in the upstream request

**`build_interactions_headers_forwards_client_auth_when_no_key`**
- GIVEN `api_key = None` and client sends `Authorization: Bearer sk-ant-...`
- WHEN `build_interactions_headers` is called
- THEN `Authorization` header IS forwarded

**`build_interactions_headers_forwards_non_auth_headers`**
- GIVEN `api_key = Some(...)` and client sends `x-request-id: trace-123`
- WHEN `build_interactions_headers` is called
- THEN `x-request-id` IS forwarded regardless of api_key presence

### E2E tests (in `tests/e2e.rs`):

**`interactions_strips_client_auth_headers_when_api_key_set`**
- GIVEN section has `api_key` and client sends `Authorization: Bearer sk-ant-...`
- WHEN request is dispatched via `InteractionsHandler`
- THEN the upstream receives only `x-goog-api-key`, not `Authorization`

**`interactions_sets_x_goog_api_key_from_config`**
- GIVEN section has `api_key = "secret-key"`
- WHEN request is dispatched
- THEN upstream receives `x-goog-api-key: secret-key`

## Scope

### In Scope
- 4 unit tests for `build_interactions_headers` (green path)
- 2 E2E tests for auth header behavior on full dispatch path (green path)

### Out of Scope
- Modifying `forward_request_headers` 
- Testing other handlers (OpenAI, Anthropic)
- Performance/load tests

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/interactions_handler.rs` | Yes | Add test module with 4 unit tests |
| `tests/e2e.rs` | Yes | Add 2 E2E tests |
| `tests/common/mod.rs` | No | Existing helpers sufficient |
| Specs | No | Already documented in `reqwest-gzip-decompression` |

## Success Criteria

- [x] 4 unit tests for `build_interactions_headers` pass
- [x] 2 E2E tests for interactions auth headers pass
- [x] All existing tests continue to pass (258 total)
- [x] `cargo fmt`, `cargo clippy --locked` pass

---

## Archive Information

**Archived:** 2026-06-21
**Duration:** 1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — +4 unit tests in test module
- `tests/e2e.rs` — +2 E2E tests for auth header verification

### Specs Updated
- `openspec/specs/routing.md` — added scenarios for non-auth header forwarding and x-goog-api-key matching
