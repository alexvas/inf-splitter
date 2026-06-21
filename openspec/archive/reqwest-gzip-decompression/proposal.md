# Proposal: Document reqwest gzip Decompression

**Change ID:** `reqwest-gzip-decompression`
**Created:** 2026-06-20
**Status:** Draft

---

## Problem Statement

Three improvements were made but not yet reflected in the OpenSpec specs:

1. **gzip decompression**: reqwest was configured with `default-features = false` without the `gzip` feature. When upstream APIs (Gemini) returned gzip-compressed error responses, `upstream.text()` returned raw binary — the `error` field in diagnostic stats showed garbled gzip bytes instead of readable JSON.

2. **Release version in startup log**: The startup log message (`"starting inf-splitter"`) did not include the release version, making it harder to identify which build is running from system logs.

3. **OVERLOADED_CREDENTIALS on interactions upstream**: `build_interactions_headers` passed `None` to `forward_request_headers`, forwarding client `Authorization` headers to Gemini alongside `x-goog-api-key`. Gemini rejected the request because two authentication methods were present.

## Proposed Solution

### Already implemented:

1. Added `"gzip"` to reqwest features in `Cargo.toml`. reqwest now sends `Accept-Encoding: gzip` and auto-decompresses upstream responses.

2. Added `version = env!("CARGO_PKG_VERSION")` to the startup `info!` log in `main.rs`.

3. Replaced the `forward_request_headers(b, request_headers, None)` call in `build_interactions_headers` with inline header forwarding that skips auth headers when `api_key` is set, preventing conflict between client auth and `x-goog-api-key`.

### Spec updates needed:

- **diagnostics.md**: Document that the `error` field in stats events contains the upstream error body decoded (not raw transport bytes). Add note about reqwest gzip support.
- **routing.md**: Update auth header forwarding for interactions dispatch — client auth headers are suppressed when `api_key` is configured.
- **project.md**: Add gzip to reqwest features in key dependencies.

## Scope

### In Scope
- Document gzip decompression behavior in diagnostics and project specs
- Document interactions auth header suppression in routing spec
- Document version field in startup log

### Out of Scope
- Code changes (already implemented)
- New tests

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| diagnostics.rs | No | Already implemented |
| interactions_handler.rs | No | Already implemented |
| main.rs | No | Already implemented |
| Cargo.toml | No | Already implemented |
| Specs | Yes | diagnostics.md, routing.md, project.md |

## Success Criteria

- [x] diagnostics.md updated with gzip/error field clarification
- [x] routing.md updated with interactions auth header behavior
- [x] project.md updated with gzip in reqwest features
- [x] All three checks pass (fmt, clippy, test)

---

## Archive Information

**Archived:** 2026-06-21
**Duration:** 1 day
**Outcome:** Successfully implemented

### Files Changed (code)
- `Cargo.toml` — added `"gzip"` to reqwest features
- `src/main.rs` — added `version` field to startup log
- `src/interactions_handler.rs` — `build_interactions_headers` strips client auth headers when `api_key` is set

### Specs Updated
- `openspec/specs/diagnostics.md` — gzip auto-decompression, error field decoded
- `openspec/specs/routing.md` — interactions auth header suppression
- `openspec/project.md` — gzip in reqwest features, version in startup log
