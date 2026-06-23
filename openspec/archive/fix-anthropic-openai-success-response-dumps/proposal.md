# Proposal: Fix Conversion Success Response Dumps

**Change ID:** `fix-anthropic-openai-success-response-dumps`
**Created:** 2026-06-23
**Status:** Implementation Complete
**Completed:** 2026-06-23

---

## Problem Statement

Successful protocol-conversion paths currently produce incomplete dump output. Operators see `ingress/request` and `egress/request`, but no `egress/response`, even though the upstream returned a successful response.

Affected paths:

| Direction | Handler | Success response dump |
|-----------|---------|:---:|
| Anthropic ingress → OpenAI upstream, non-streaming | `OpenAiHandler::handle_sync_manual` | Missing |
| Anthropic ingress → OpenAI upstream, streaming | `OpenAiHandler::handle_stream_manual` | Missing |
| OpenAI ingress → Anthropic upstream, non-streaming | `AnthropicHandler::handle_from_openai` | Missing |
| OpenAI ingress → Anthropic upstream, streaming | `AnthropicHandler::handle_from_openai_stream` | Missing |

Passthrough success paths use relay helpers that record response dumps, and upstream error paths use `finish_with_upstream_error`. The gap is the successful conversion paths that consume upstream responses for translation without first recording the raw upstream body.

## Proposed Solution

Record response dumps on all successful protocol-conversion paths:

- Non-streaming conversion should read upstream response bytes once, record the raw upstream body and headers via the `RequestDiagnostics` guard, then deserialize/translate from those same bytes.
- Streaming conversion should accumulate raw upstream SSE bytes up to the existing streaming dump limit while translating the stream, then record the raw upstream stream and headers when the stream completes.
- Guard finalization for streaming conversion should happen when the stream actually finishes, not immediately after constructing the response body.
- Existing passthrough and upstream error behavior remains unchanged.
- Interactions streaming client-disconnect behavior remains unchanged, but the implementation should add a short comment explaining why no response dump is recorded before finalizing with status 499.

## Scope

### In Scope

- Add failing regression tests for successful non-streaming Anthropic→OpenAI and OpenAI→Anthropic conversion dumps.
- Add or identify streaming conversion coverage for response dump finalization in both directions.
- Update `src/openai.rs` success paths to record `egress/response` dumps with upstream response headers.
- Update `src/anthropic.rs` success paths to record `egress/response` dumps with upstream response headers.
- Preserve shared `request_id` across ingress request, egress request, egress response, and stats events.

### Out of Scope

- Changing dump configuration format.
- Changing passthrough response relay behavior.
- Changing interactions handler behavior except for shared helpers if strictly needed.
- Redesigning streaming diagnostics beyond the existing 1 MiB capture limit and SSE dump format rules.

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| Database | No | No persistence schema changes |
| API | No | Proxy HTTP API remains unchanged |
| State | No | Only diagnostic side effects change |
| UI | No | No UI exists |
| Diagnostics | Yes | Conversion success paths must emit missing response dumps |
| Tests | Yes | Regression tests for both conversion directions |

## Architecture Considerations

This change should reuse the existing `RequestDiagnostics` guard rather than adding direct `Diagnostics::record_*` calls. Non-streaming response dumping must avoid double-consuming the upstream response body. Streaming response dumping must move or share the guard with stream processing so response dumps and stats are emitted when the stream reaches completion, preserving stats/dump parity.

## Success Criteria

- [ ] Successful non-streaming Anthropic→OpenAI conversion with `dump_mode = "all"` writes `ingress/request`, `egress/request`, and `egress/response` dump lines.
- [ ] Successful non-streaming OpenAI→Anthropic conversion with `dump_mode = "all"` writes `ingress/request`, `egress/request`, and `egress/response` dump lines.
- [ ] Response dumps contain raw upstream response bodies and upstream response headers.
- [ ] All dump lines and the stats line for each request share the same `request_id`.
- [ ] Streaming conversion success records response dumps when streams complete, or the task documents why existing lower-level coverage is sufficient.
- [ ] `cargo fmt --check`, `cargo clippy --locked -- -D warnings`, and `cargo test --locked` pass.

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Consuming the upstream body twice on non-streaming paths | Med | High | Read bytes once, dump from those bytes, then deserialize from the same buffer |
| Finalizing streaming guard before stream completion | High | High | Move finalization into the stream wrapper/task that observes EOF/error |
| Dumping translated response instead of raw upstream response | Low | Med | Add regression assertions against upstream-shaped bodies |
| Streaming refactor affects client-visible SSE translation | Med | High | Keep existing translation state machines and only add side-channel capture/finalization |

---

## Archive Information

**Archived:** 2026-06-23 23:15
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/openai.rs` — `handle_sync_manual` reads bytes before deserialization, records response dump; `handle_stream_manual` captures raw upstream SSE, moves guard finalization into stream completion
- `src/anthropic.rs` — `handle_from_openai` reads bytes before deserialization, records response dump; `handle_from_openai_stream` captures raw upstream SSE, moves guard finalization into stream completion
- `src/diagnostics.rs` — made `finish_with_error` `pub(crate)` for streaming error paths
- `src/interactions_handler.rs` — added comment on client-disconnect path explaining missing response dump
- `tests/protocol_conversion.rs` — 4 regression tests for conversion success egress/response dumps

### Specs Updated
- `openspec/specs/diagnostics.md` — added "Protocol Conversion Success Paths Record Upstream Response Dumps", "Interactions Client Disconnect Response Dump Exception Is Documented" requirements; added scenarios to "Every Protocol Handler Records Dump Events" and "Every Protocol Handler Records Stats Events"
- `openspec/specs/protocol-conversion.md` — added "Conversion Handlers Preserve Raw Upstream Responses for Diagnostics" requirement
