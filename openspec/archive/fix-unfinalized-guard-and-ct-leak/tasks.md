# Implementation Tasks: Fix Unfinalized Guard & Content-Type Header Leak

**Change ID:** `fix-unfinalized-guard-and-ct-leak`

---

## Phase 1: `abort_*` API (diagnostics.rs)

- [x] 1.1 Add private `abort_with()` helper — calls `finish_with_error`, then constructs `AppError` via closure
- [x] 1.2 Add `abort_upstream()` → `AppError::Upstream` (HTTP 502)
- [x] 1.3 Add `abort_internal()` → `AppError::Internal` (HTTP 500)
- [x] 1.4 Add `abort_bad_request()` → `AppError::BadRequest` (HTTP 400)
- [x] 1.5 Remove `pub` from `finish_with_error`
- [x] 1.6 Add unit tests: `abort_upstream_finalizes_guard_and_returns_upstream_error`, `abort_internal_returns_internal_error`, `abort_bad_request_returns_bad_request`, `abort_is_idempotent`

**Quality Gate:**
- [x] Unit tests pass (268)

---

## Phase 2: Exclude content-type (auth.rs)

- [x] 2.1 Add `"content-type"` to hop-by-hop exclusion list in `should_forward_request_header()`
- [x] 2.2 Update `should_forward_excludes_hop_by_hop` test — assert content-type IS excluded
- [x] 2.3 Update `forward_request_headers_map_with_api_key` test — assert content-type is None

**Quality Gate:**
- [x] Unit tests pass

---

## Phase 3: Fix guards in openai.rs

- [x] 3.1 `handle_from_openai`: `to_vec?` (line 113) → `.map_err(|e| guard.abort_internal(...))?`
- [x] 3.2 `handle_from_openai`: `send().await?` (line 134) → `.map_err(|e| guard.abort_upstream(...))?`
- [x] 3.3 `handle_from_openai`: `relay_openai_upstream().await?` (line 173) → `.map_err(|e| guard.abort_upstream(...))?`
- [x] 3.4 `relay_openai_upstream`: add `upstream_url` and `direction` params, fix internal `bytes().await?` and `validate_upstream_body()?` with `abort_upstream`
- [x] 3.5 `handle_sync_manual`: `send().await?`, `json().await?` → `abort_upstream`
- [x] 3.6 `handle_stream_manual`: `send().await?` → `abort_upstream`, `sse_response()?` → `abort_internal`

**Quality Gate:**
- [x] Integration tests pass (63)

---

## Phase 4: Fix guards in anthropic.rs

- [x] 4.1 `handle_from_anthropic`: `to_vec?`, `build_upstream_request()?`, `send().await?`, `relay_upstream_response().await?` → `abort_*`
- [x] 4.2 `relay_upstream_response`: add `upstream_url` and `direction` params, fix internal `bytes().await?` and `validate_upstream_body()?` with `abort_upstream`
- [x] 4.3 `handle_from_openai`: `build_upstream_request()?`, `send().await?`, `json().await?` → `abort_*`
- [x] 4.4 `handle_from_openai_stream`: `build_upstream_request()?`, `send().await?`, `sse_response()?` → `abort_*`

**Quality Gate:**
- [x] Integration tests pass

---

## Phase 5: Fix guards in interactions_handler.rs

- [x] 5.1 `handle_from_anthropic`: `to_vec?` → `abort_internal`
- [x] 5.2 `handle_from_openai`: `to_vec?` → `abort_internal`
- [x] 5.3 `handle_split_send`: loop body hazards (5 `?` sites) + post-loop (3 `?` sites) → `abort_*`
- [x] 5.4 `send_split_system_instruction`: sys-parts loop (4 `?`), remaining-chunks loop (4 `?`), post-loop (3 `?`) → `abort_*`
- [x] 5.5 `send_and_translate`: 4 `finish_with_error` + `return Err(...)` → `return Err(guard.abort_upstream(...))`
- [x] 5.6 `send_and_translate`: build_response failure → `abort_internal`
- [x] 5.7 `handle_control_action`: CleanAll + ExtendLifetime → `abort_internal`
- [x] 5.8 `can_split_under_limit` failures → `abort_bad_request`
- [x] 5.9 `pack_content_into_chunks` / `split_text_for_limit` failures → `abort_bad_request`
- [x] 5.10 Streaming error handlers inside `tokio::spawn` → `let _ = guard.abort_upstream(...)`

**Quality Gate:**
- [x] Integration tests pass

---

## Phase 6: Integration & Polish

- [x] 6.1 `cargo fmt` — clean
- [x] 6.2 `cargo clippy --locked -- -D warnings` — clean
- [x] 6.3 `cargo test --locked` — all 359 pass
- [x] 6.4 Verify no `pub fn finish_with_error` remains
- [x] 6.5 Verify zero `guard.finish_with_error` calls outside `diagnostics.rs`

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] 359 tests pass, clippy clean, fmt clean
- [ ] Ready for `/openspec-archive`
