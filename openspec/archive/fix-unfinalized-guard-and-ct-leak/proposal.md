# Proposal: Fix Unfinalized Guard & Content-Type Header Leak

**Change ID:** `fix-unfinalized-guard-and-ct-leak`
**Created:** 2026-06-23
**Status:** Draft

---

## Problem Statement

Two bugs discovered during investigation of `deepseek` and `codex` section failures:

1. **Guard dropped without finish** — 31 `?` operators across `openai.rs`, `anthropic.rs`, and `interactions_handler.rs` caused `RequestDiagnostics` guard to be dropped without calling `finish()`/`finish_with_error()`, violating the "No Unfinalized Guard on Error Return" invariant from `diagnostics.md`. The existing pattern required verbose `match` blocks at every fallible call site.

2. **Duplicate `Content-Type` header** — `forward_request_headers_map()` in `auth.rs` forwarded the ingress `content-type` header to upstream. Each handler also set `Content-Type: application/json` explicitly on the builder. `reqwest::header()` appends rather than replaces, producing two `Content-Type` headers on the wire. Confirmed via diagnostic curl: duplicate `Content-Type` causes OpenAI to return `"you must provide a model parameter"` — the request body is not parsed.

## Proposed Solution

### Part 1: `abort_*` helper methods on `RequestDiagnostics`

Replace the verbose `guard.finish_with_error(...) + return Err(...)` pattern with compact `return Err(guard.abort_xxx(...))`:

| Method | Returns | HTTP | Use case |
|--------|---------|------|----------|
| `abort_upstream(dur, size, up, dir, streaming, err)` | `AppError::Upstream` | 502 | Network/upstream errors |
| `abort_internal(dur, size, up, dir, streaming, err)` | `AppError::Internal` | 500 | Serialization/response construction |
| `abort_bad_request(dur, size, up, dir, streaming, err)` | `AppError::BadRequest` | 400 | Validation errors |

Internally, all three delegate to a private `abort_with()` helper which calls `finish_with_error()`.

`finish_with_error()` is demoted from `pub` to private — the only external way to finalize-with-error a guard is through one of the three `abort_*` methods.

### Part 2: Exclude `content-type` from forwarded headers

Add `"content-type"` to the exclusion list in `should_forward_request_header()` in `auth.rs`. Handlers already set `Content-Type: application/json` explicitly; forwarding the ingress value caused the duplicate.

## Scope

### In Scope
- Add `abort_upstream`, `abort_internal`, `abort_bad_request` methods on `RequestDiagnostics`
- Make `finish_with_error` private
- Replace all bare `?` operators after guard creation with `.map_err(|e| guard.abort_xxx(...))?`
- Replace all `guard.finish_with_error(...)` + `return Err(...)` with `return Err(guard.abort_xxx(...))`
- Exclude `content-type` from request header forwarding
- Update unit tests for auth.rs and diagnostics.rs

### Out of Scope
- Changing `finish_with_upstream_error` (still needed for upstream HTTP errors with response body dumps)
- Protocol-level changes beyond header forwarding

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `diagnostics.rs` | Yes | +3 public methods, +1 private helper, `finish_with_error` → private |
| `auth.rs` | Yes | 1 line: exclude `content-type` from forwarding |
| `openai.rs` | Yes | 9 `?` sites → `.map_err(\|e\| guard.abort_xxx(...))?` |
| `anthropic.rs` | Yes | 10 `?` sites → `.map_err(\|e\| guard.abort_xxx(...))?` |
| `interactions_handler.rs` | Yes | 20 `?` sites + 15 `finish_with_error` blocks unified |
| Tests | Yes | Updated auth content-type test, added abort tests |

## Architecture Considerations

- The `abort_*` pattern is a natural extension of the existing `RequestDiagnostics` guard — it enforces the "no unfinalized guard on error return" invariant at the API level by making `?` safe
- The private `abort_with` helper takes a closure `FnOnce(String) -> AppError` for constructing the correct error variant, keeping the three public methods thin
- `content-type` belongs in the hop-by-hop exclusion list alongside `connection`, `transfer-encoding`, etc.

## Success Criteria

- [ ] All 359 tests pass
- [ ] Clippy clean
- [ ] No `guard.finish_with_error` calls remain outside `diagnostics.rs`
- [ ] No `?` operator after guard creation without `.map_err(|e| guard.abort_xxx(...))`
- [ ] Restart inf-splitter: no "diagnostics guard dropped without finish" in logs
- [ ] Restart inf-splitter: OpenAI "model parameter" error resolved

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| HTTP status change for some errors | Low | Low | Test coverage confirms expected statuses; 502 for upstream errors is more correct than 500 |
| `abort_*` loses `response_size` detail | Low | Low | `finish_with_error` still accepts `response_size` via `finish_with_upstream_error`; internal errors don't have a response body |

---

## Archive Information

**Archived:** 2026-06-23 22:42
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/diagnostics.rs` — +3 public `abort_*` methods, +1 private `abort_with`, `finish_with_error` → private
- `src/auth.rs` — `content-type` excluded from `should_forward_request_header`
- `src/openai.rs` — 9 `?` sites fixed via `.map_err(|e| guard.abort_xxx(...))?`
- `src/anthropic.rs` — 10 `?` sites fixed
- `src/interactions_handler.rs` — 20 `?` sites + 15 explicit `finish_with_error` blocks unified
- `tests/protocol_conversion.rs` — status assertions updated (500 → 502 for non-UTF8 upstream)

### Specs Updated
- `openspec/specs/diagnostics.md` — added `abort_*` API, updated invariant, added content-type exclusion
