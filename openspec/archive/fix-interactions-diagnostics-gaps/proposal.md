# Proposal: Fix Interactions Diagnostics Gaps

**Change ID:** `fix-interactions-diagnostics-gaps`
**Created:** 2026-06-23
**Status:** Implementation Complete
**Completed:** 2026-06-23

---

## Problem Statement

Four related bugs in the interactions handler diagnostics, session persistence, and deployment cause data loss and spurious errors:

1. **Response dump headers always empty** — `response_dump` and `response_dump_streaming` are called with `vec![]` instead of actual upstream response headers across 12 call sites in `interactions_handler.rs` (9), `anthropic.rs` (1 error path), `openai.rs` (1 error path), and `diagnostics.rs` (1 hardcoded in `response_dump_streaming`). In `anthropic.rs:96` and `openai.rs:141` the response headers are already captured but discarded. Operators cannot inspect upstream response headers (e.g., rate-limit indicators, tracking IDs) in dump files.

2. **"diagnostics guard dropped without finish" on control action failure** — In `handle_control_action`, when `remove_all()` or `extend_lifetime()` fails, the `?` operator returns early before `guard.finish()` is called. The guard's `Drop` impl then logs an error and records a degraded stats entry with `status: 0`.

3. **Session `save_to_disk` fails with ENOENT, errors silently lost at all 5 `update` call sites** — `save_to_disk` writes to a `.tmp` file via `fs::write` but never creates the parent directory. The default path is `/var/lib/inf-splitter/interactions-sessions.toml`. If the directory doesn't exist, `fs::write` fails with "No such file or directory". All 5 call sites of `session_store.update()` silently discard the error with `let _ =`: non-streaming success (line 474), streaming eager commit (line 533), streaming completion (line 695), split-send completion (line 864), and system-instruction-split completion (line 1049). The first successful request creates an in-memory session but never persists it. When `remove_all` later calls `save_to_disk`, the error propagates and causes a 500.

4. **`/var/lib/inf-splitter/` not created at package install time** — The `.deb` `postinst` creates `/var/log/inf-splitter` but not `/var/lib/inf-splitter`. With `ProtectSystem=strict` in the systemd unit, the service cannot write to `/var/lib` at all unless the path is explicitly listed in `ReadWritePaths`. This means even with the `create_dir_all` runtime fix, a fresh `.deb` install won't work because systemd blocks the write.

## Proposed Solution

1. **Capture and pass upstream response headers** in all response dump calls. Extract headers from `reqwest::Response::headers()` at each call site in `interactions_handler.rs` (9 sites). Fix already-captured-but-discarded headers in `anthropic.rs:96` and `openai.rs:141` (pass the existing `response_headers`/`headers` variable instead of `vec![]`). Add `headers` parameter to `response_dump_streaming`.

2. **Call `guard.finish_with_error()` before `?`** in `handle_control_action` error paths for both `CleanAll` and `ExtendLifetime` variants.

3. **Create parent directory in `save_to_disk`** using `std::fs::create_dir_all` before writing. **Embed `tracing::warn!` into `SessionStore::update()`** — log the error inside the method instead of returning it, so persistence failures are always surfaced regardless of how callers handle the result. The 5 existing `let _ =` call sites remain unchanged (non-fatal by design).

4. **Create `/var/lib/inf-splitter/` in `debian/postinst`** and **add `ReadWritePaths=/var/lib/inf-splitter` to the systemd unit** so the runtime fix actually works on a fresh `.deb` install.

## Scope

### In Scope
- Fix response dump headers in `interactions_handler.rs` (9 call sites), `anthropic.rs` (1 error path), `openai.rs` (1 error path)
- Add `headers` parameter to `diagnostics.rs` `response_dump_streaming`
- Fix guard finish in `handle_control_action` error paths (2 variants)
- Fix `save_to_disk` to create parent directory
- Embed `tracing::warn!` into `SessionStore::update()` — log error inside the method instead of returning it
- Create `/var/lib/inf-splitter/` in `debian/postinst`
- Add `ReadWritePaths=/var/lib/inf-splitter` to systemd unit
- Add test coverage for the fixed paths

### Out of Scope
- Control message idempotency across requests (separate feature)
- Session file path configurability

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions_handler.rs` | Yes | Capture upstream headers (9 sites), fix guard finish in error paths, log update errors |
| `session.rs` | Yes | `create_dir_all` in `save_to_disk`, change `update` error handling |
| `diagnostics.rs` | Yes | Add `headers` parameter to `response_dump_streaming` |
| `anthropic.rs` | Yes | Pass captured `response_headers` instead of `vec![]` at line 96 |
| `openai.rs` | Yes | Pass captured `upstream.headers()` instead of `vec![]` at line 141 |
| `debian/postinst` | Yes | Add `mkdir -p /var/lib/inf-splitter` with chown |
| `debian/inf-splitter.service` | Yes | Add `ReadWritePaths=/var/lib/inf-splitter` |
| Specs | Yes | Update diagnostics.md delta, add deployment.md delta |

## Architecture Considerations

All three fixes are local and follow existing patterns:
- Response headers extraction matches `egress_dump` pattern where headers are already captured via `build_interactions_headers_map`
- Guard finish matches the existing pattern used on all other error paths in the same file
- `create_dir_all` is idempotent and cheap — called on every `save_to_disk` but safe since it's a no-op when the directory exists

## Success Criteria

- [ ] Response dump entries in NDJSON contain actual upstream response headers (not empty array)
- [ ] Control action failures produce proper stats entries with the error message (not "diagnostics guard dropped without finish")
- [ ] Session persistence works when `/var/lib/inf-splitter/` does not exist on first run
- [ ] `update` failures are logged; `remove_all` still propagates errors correctly
- [ ] Fresh `.deb` install creates `/var/lib/inf-splitter/` with correct ownership
- [ ] systemd unit allows writes to `/var/lib/inf-splitter`
- [ ] `cargo test --locked` passes
- [ ] `cargo clippy --locked -- -D warnings` passes

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Changing `update` signature breaks callers | Low | Low | Only 3 call sites, all in `interactions_handler.rs` |
| `create_dir_all` on every save is wasteful | Low | Low | It's a few syscalls; session saves are infrequent (once per request) |

---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/session.rs` — `create_dir_all` in `save_to_disk`, `tracing::warn!` via `inspect_err` in `update`, test
- `src/diagnostics.rs` — `response_dump_streaming` now accepts `headers` parameter
- `src/interactions_handler.rs` — guard `finish_with_error` before `?` in `handle_control_action`, `response_headers_to_pairs` helper, 9 response dump sites pass actual headers
- `src/anthropic.rs` — error path passes `response_headers.clone()` instead of `vec![]`
- `src/openai.rs` — error path converts and passes upstream headers
- `debian/postinst` — creates `/var/lib/inf-splitter` with correct ownership
- `debian/inf-splitter.service` — `ReadWritePaths` includes `/var/lib/inf-splitter`

### Specs Updated
- `openspec/specs/diagnostics.md` — updated "Egress and Response Dumps Use Actual Upstream Headers", added control action error scenarios, "Session Store Creates Parent Directory on Save", "Session Update Errors Are Logged"
- `openspec/specs/deployment.md` — updated "Linux Package" requirement with session directory
