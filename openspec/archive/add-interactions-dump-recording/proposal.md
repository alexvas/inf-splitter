# Proposal: Every protocol handler records dump events

**Change ID:** `add-interactions-dump-recording`
**Created:** 2026-06-21
**Status:** Complete

---

## Problem Statement

The invariant "every protocol handler records dump events" was violated: the interactions handler (`interactions_handler.rs`) recorded diagnostic stats events (written to `diag-*.ndjson`) but never recorded dump events (written to `dump-*.ndjson`). The Anthropic and OpenAI handlers already recorded ingress, egress, and response dumps in all paths; the interactions handler was the only one missing this.

## Proposed Solution

Add dump event recording to `send_and_translate` and `handle_stream_response` in `interactions_handler.rs`, following the same pattern used by the Anthropic and OpenAI handlers:

1. **Ingress request dump**: record the original client body (Anthropic `MessageCreateRequest` or OpenAI `ChatCompletionRequest`) before any processing
2. **Egress request dump**: record the interactions request body (`CreateModelInteractionParams` serialized to JSON) actually sent upstream
3. **Response dump (non-streaming)**: record the raw upstream response body
4. **Response dump (streaming)**: buffer raw SSE bytes (up to 1 MiB) in the spawned task, record on stream completion
5. **Response dump (error)**: record the upstream error body on non-2xx responses
6. **Shared request_id**: use a single `request_id` for all dump and stats events associated with one request

## Scope

### In Scope
- Dump event recording in `send_and_translate` for ingress, egress, and response (success + error)
- Dump event recording in `handle_stream_response` with buffered streaming response body
- Dump event recording in `handle_split_send` (proxy_limit splitting path)
- Dump event recording in `send_split_system_instruction`
- Passing `ingress_body: &[u8]` through the call chain instead of just `request_size: usize`
- Non-UTF8 detection with `tracing::warn!` and base64 fallback (matching existing handlers)

### Out of Scope
- (none)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions_handler.rs` | Yes | Add dump recording calls, signature changes |
| `diagnostics.rs` | No | Existing API used as-is |
| Tests | Yes | Add tests verifying dump file is created and contains events |

## Architecture Considerations

Establishes a universal invariant: every protocol handler must record dump events for every request. Follows the existing pattern from `anthropic.rs` and `openai.rs`:
- Use `crate::diagnostics::dump_body_from_bytes()` for body conversion
- Use `diagnostics.record_request_dump()` for ingress/egress requests
- Use `diagnostics.record_response_dump()` for responses
- Use `crate::relay::MAX_STREAMING_DUMP_BYTES` (1 MiB) for streaming buffer limit
- Use `diagnostics.dump_enabled()` guard to avoid unnecessary allocations
- Use `tracing::warn!` for non-UTF8 body detection

## Success Criteria

- [x] `dump-gemini.ndjson` (or equivalent per-section dump file) contains ingress, egress, and response dump lines when `dump_mode = "all"`
- [x] Dump lines share the same `request_id` as the corresponding stats line
- [x] Streaming responses have their body captured in the dump (up to 1 MiB)
- [x] Error responses have their body captured in the dump
- [x] Non-UTF8 bodies are base64-encoded with appropriate warning
- [x] `cargo fmt --check`, `cargo clippy`, and `cargo test` pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Memory overhead from dump buffer in streaming path | Low | Low | Limited to `MAX_STREAMING_DUMP_BYTES` (1 MiB), guarded by `dump_enabled()` |
| Channel overflow dropping dump events | Low | Low | Same `try_send` semantics as other handlers — silent drop is intentional |

---

## Archive Information

**Archived:** 2026-06-21
**Duration:** 1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — dump recording in all paths (+117 lines)
- `tests/protocol_conversion.rs` — 5 new interactions dump tests (+381 lines)

### Specs Updated
- `openspec/specs/diagnostics.md` — added "Every Protocol Handler Records Dump Events" requirement
