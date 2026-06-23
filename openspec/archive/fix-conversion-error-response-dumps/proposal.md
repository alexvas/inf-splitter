# Proposal: Fix Conversion Error Response Dumps

**Change ID:** `fix-conversion-error-response-dumps`
**Created:** 2026-06-23
**Status:** Implementation Complete
**Completed:** 2026-06-23

---

## Problem Statement

When an upstream returns an error in the protocol-conversion paths (Anthropic↔OpenAI), the response body is not dumped. Only request dumps (ingress + egress) appear in the ndjson dump file, with the HTTP status code overlaid onto the request entries. The upstream error body — containing the actual error message — is lost, making debugging impossible.

Root cause: `response_dump()` is called before `finish_with_error()` at some call sites but forgotten at others. The pattern is a manual two-call sequence with no compile-time enforcement.

## Current Behavior

8 call sites handle upstream HTTP errors. 4 are missing `response_dump`:

| File | Method | `response_dump` |
|------|--------|:---:|
| `openai.rs` | `handle_from_openai` (passthrough) | ✓ |
| `openai.rs` | `handle_sync_manual` (conversion) | **✗** |
| `openai.rs` | `handle_stream_manual` (conversion) | **✗** |
| `anthropic.rs` | `handle_from_anthropic` (passthrough) | ✓ |
| `anthropic.rs` | `handle_from_openai` (conversion) | **✗** |
| `anthropic.rs` | `handle_from_openai_stream` (conversion) | **✗** |
| `interactions_handler.rs` | 4 upstream error paths | ✓ |

8 additional call sites are internal errors (validation, session, stream infrastructure) — these correctly do NOT call `response_dump` (no HTTP response body exists).

## Proposed Solution

Add `RequestDiagnostics::finish_with_upstream_error()` — a single method that encapsulates `response_dump` + `finish_with_error`. Replace all 8 upstream HTTP error call sites with it, making the bug impossible by construction:

```rust
/// Record an upstream HTTP error: dumps the error response body, then finishes with error stats.
pub fn finish_with_upstream_error(
    &self,
    status: u16,
    duration_ms: u64,
    request_size: usize,
    upstream: &str,
    direction: &str,
    streaming: bool,
    error_body: String,
    response_headers: Vec<(String, String)>,
) {
    self.response_dump(
        dump_body_from_bytes(error_body.as_bytes()),
        status, true, response_headers,
    );
    self.finish_with_error(
        status, duration_ms, request_size,
        Some(error_body.len()),
        upstream, direction, streaming, error_body,
    );
}
```

Call sites shrink from 2 calls to 1:

```rust
// Before (easy to forget response_dump):
guard.response_dump(dump_body_from_bytes(error_body.as_bytes()), status, true, headers);
guard.finish_with_error(status, duration, size, Some(body.len()), upstream, dir, stream, error_body);

// After (impossible to forget):
guard.finish_with_upstream_error(status, duration, size, upstream, dir, stream, error_body, headers);
```

## Scope

### In Scope
- `diagnostics.rs`: add `finish_with_upstream_error` method to `RequestDiagnostics`
- `openai.rs`: replace 1 correct + 2 broken call sites (3 total)
- `anthropic.rs`: replace 1 correct + 2 broken call sites (3 total)
- `interactions_handler.rs`: replace 4 correct call sites (4 total)

### Out of Scope
- Success-path response dumps
- Internal error paths that use `finish_with_error` without upstream HTTP body (8 call sites — unchanged)
- `finish()` success path

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `diagnostics.rs` | Yes | Add `finish_with_upstream_error` method (~12 lines) |
| `openai.rs` | Yes | Replace 3 call sites |
| `anthropic.rs` | Yes | Replace 3 call sites |
| `interactions_handler.rs` | Yes | Replace 4 call sites |
| Config | No | No config changes |

## Success Criteria

- [ ] `finish_with_upstream_error` method added to `RequestDiagnostics`
- [ ] All 10 upstream HTTP error call sites use the new method
- [ ] All 8 internal error call sites unchanged (still use `finish_with_error`)
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --locked -- -D warnings` passes
- [ ] `cargo test --locked` passes
- [ ] Dump ndjson for a failing conversion request contains a response entry with `"direction":"response"` and the upstream error body

---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/diagnostics.rs` — added `finish_with_upstream_error` method + 2 unit tests
- `src/openai.rs` — 3 call sites replaced (2 bugfixes, 1 refactor)
- `src/anthropic.rs` — 3 call sites replaced (2 bugfixes, 1 refactor)
- `src/interactions_handler.rs` — 4 call sites replaced (refactor)

### Specs Updated
- `openspec/specs/diagnostics.md` — added `finish_with_upstream_error` to RequestDiagnostics methods, added 4 new scenarios, updated Dump Event Coverage scenarios
