# Proposal: Fix Split-Send Streaming Response

**Change ID:** `fix-split-send-streaming-response`
**Created:** 2026-06-23
**Status:** Archived

---

## Problem Statement

Three bugs in the proxy_limit split-send path:

### Bug 1: Non-streaming response to streaming client

`handle_split_send` and `send_split_system_instruction` always return non-streaming JSON via `ok_with_session_header`, ignoring the `_stream` parameter. When Claude Code sends `stream: true`, it expects `text/event-stream` SSE events. Receiving `application/json` instead causes:

> API Error: Stream ended without receiving any events

### Bug 2: Chunks measured by content only, not full body

`split_content_for_limit` measures only the `input` content array, ignoring the serialized envelope (`model`, `stream`, `system_instruction`, `tools`, `generation_config`, `previous_interaction_id`). A chunk that "fits" by content size can exceed `proxy_limit` when serialized as a full `CreateModelInteractionParams`. In the observed ping dump: content is 1 tiny message, but the full chunk body is 128KB (limit: 100k) due to `system_instruction` (27KB) + `tools` (~84KB) overhead.

### Bug 3: Missing ingress response dump in split-send path

The diagnostics dump for the ping request contains `"stage":"egress","direction":"response"` (line 3 — upstream response from Gemini), but no `"stage":"ingress","direction":"response"` — the final translated response sent back to the client is never dumped. Both `handle_split_send` and `send_split_system_instruction` call `guard.response_dump()` for each chunk's upstream response, but the final ingress response (JSON or SSE) built by `ok_with_session_header` or the new SSE path is not recorded.

## Proposed Solution

Реализация через **red-green (TDD)**: каждый шаг начинается с падающего теста, доказывающего проблему, затем минимальная реализация.

### Fix 1: Return SSE events from split-send when ingress was streaming

In `handle_split_send` and `send_split_system_instruction`, when `_stream` is `true`, synthesize SSE events from the final `Interaction` (via `build_response_from_interaction` → `StreamEvent` items) and return `text/event-stream`.

- Anthropic ingress: `message_start` → `content_block_start` → `content_block_delta` → `content_block_stop` → `message_delta` → `message_stop`
- OpenAI ingress: `ReverseStreamingTranslator` → `ChatCompletionChunk` SSE + `data: [DONE]`

### Fix 2: Greedy chunk packing by full serialized size

Replace `split_content_for_limit` with a packing algorithm that measures the **full serialized chunk body** against `proxy_limit`:

**Envelope** — fields present in every chunk regardless of position:
- First chunk: `model`, `stream: false`, `tools?`, `generation_config?`, `system_instruction?`
- Subsequent chunks: `model`, `stream: false`, `previous_interaction_id`

**Algorithm (two-phase greedy):**

**Phase 1 — System instruction splitting:**
If `system_instruction` + envelope > limit:
1. Split `system_instruction` text via `split_text_for_limit` into parts where each part's serialized chunk ≤ limit
2. Send each part as a separate interaction with empty input, chained via `previous_interaction_id`
3. The last system-instruction chunk carries `tools` and `generation_config` (if first interaction)

**Phase 2 — Content packing:**
After system_instruction is delivered (or if it fit without splitting):
1. For each content item, measure the serialized chunk size with the item added
2. If it fits — greedily add it to the current chunk
3. If it doesn't fit — finalize the current chunk, start a new one
4. If a single content item alone exceeds the limit → error (`can_split_under_limit` already catches this)

**Invariants:**
- Every serialized chunk body ≤ `proxy_limit`
- Each chunk is as large as possible (greedy)
- System instruction is consumed first (empty user content), then user content packed greedily

### Fix 3: Dump ingress response in split-send path

After building the final client response (JSON or SSE), call `guard.response_dump()` with `stage: "ingress"` and `direction: "response"` before returning. This mirrors what `send_and_translate` implicitly does via `guard.finish()` — the split-send path must explicitly dump the translated response body.

For the streaming path (Fix 1), the SSE body is accumulated during synthesis and dumped once. For the non-streaming path, the JSON body is serialized and dumped.

## Scope

### In Scope
- `handle_split_send`: produce SSE events when `_stream` is true
- `send_split_system_instruction`: same
- New greedy packing function replacing ad-hoc size checks
- `split_content_for_limit` replaced with full-chunk measurement
- `split_text_for_limit` stays (unchanged, used for system_instruction text splitting)
- Dump ingress response body in split-send paths (both streaming and non-streaming)
- Unit tests for streaming response, greedy packing, and ingress response dump

### Out of Scope
- Changing chunk send protocol (chunks remain non-streaming)
- `handle_stream_response` (non-split streaming path — already correct)
- `build_chunk_request` / `build_request_body` behavior

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/interactions_handler.rs` | Yes | SSE response from `handle_split_send` / `send_split_system_instruction`; new greedy packing logic |
| `src/interactions.rs` | Yes | Replace `split_content_for_limit` with full-chunk greedy packer |
| `src/sse.rs` | Possibly | Helper to convert `MessageResponse` → `Vec<StreamEvent>` |

## Architecture Considerations

- **Full-chunk measurement:** `serialized_chunk_size(envelope, content, system_instruction)` computes `serde_json::to_vec(&chunk_req).len()` — the actual wire size
- **Envelope pre-computation:** The envelope size for first chunk (with tools, gen_config) and subsequent chunks (minimal) can be measured once, then `+ input size` estimates the total
- **Greedy invariant:** The packing algorithm terminates with each chunk ≤ limit and as full as possible; proven by construction (each item is added only if it fits)
- `build_response_from_interaction` is already shared across all three response paths — synthetic SSE events reuse it

## Success Criteria

- [ ] `handle_split_send` returns SSE when `_stream` is true
- [ ] `send_split_system_instruction` returns SSE when `_stream` is true
- [ ] Every serialized chunk body ≤ `proxy_limit`
- [ ] Chunks are maximally packed (greedy)
- [ ] System instruction consumed first, then user content
- [ ] The ping request (128KB, limit=100k) no longer errors
- [ ] `cargo fmt --check`, `cargo clippy`, `cargo test` all pass
- [ ] Non-streaming split-send unchanged (still JSON)
- [ ] Diagnostics dump contains `"stage":"ingress","direction":"response"` for split-send path

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Synthetic SSE events missing fields | Low | Medium | Derive from `build_response_from_interaction` — same output as non-streaming path |
| Greedy packing changes chunk count for existing requests | Medium | Low | Only affects requests exceeding limit; the old behavior produced oversized chunks that likely failed at the upstream |
| `split_text_for_limit` unchanged — system_instruction splitting already works | Low | Low | Existing tests for system_instruction splitting pass |

---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions.rs` — `pack_content_into_chunks()` + `build_pack_body()` (greedy full-chunk packing), 4 unit tests
- `src/interactions_handler.rs` — SSE response from split-send paths, `stream` parameter wired, `ingress_response_dump()`, `streaming_response_from_interaction()`, 11 `StreamEvent` factory functions, 3 OpenAI SSE factory functions
- `src/sse.rs` — `format_sse_events()` helper
- `src/diagnostics.rs` — `ingress_response_dump_pending` field + `ingress_response_dump()` method

### Specs Updated
- `openspec/specs/protocol-conversion.md` — Proxy-Limit Split-Send Chunk Forwarding (updated with greedy packing, streaming response, full-chunk measurement), Greedy Chunk Packer (new), Synthetic SSE Events (new), Isolate JSON Roundtrip Construction (replaced Document JSON Roundtrip Usage)
