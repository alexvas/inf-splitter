# Delta: Protocol Conversion

**Change ID:** `fix-split-send-streaming-response`
**Affects:** `src/interactions_handler.rs`, `src/interactions.rs`, `src/sse.rs`

---

## MODIFIED

### Requirement: Proxy-Limit Split-Send Chunk Forwarding

When a request exceeds `proxy_limit`, content is split into chunks and sent sequentially via `handle_split_send`. Chunks are packed greedily by **full serialized body size** — not content-only measurement.

**Envelope** — per-chunk overhead from non-content fields:
- First chunk: `{model, stream: false, tools?, generation_config?, system_instruction?}`
- Subsequent chunks: `{model, stream: false, previous_interaction_id}`

**Two-phase greedy algorithm:**

**Phase 1 — System instruction:**
If `serialize(envelope_first + system_instruction + empty_input) > limit`, split `system_instruction` text via `split_text_for_limit`. Each part is sent as a separate chunk (with empty input), chained via `previous_interaction_id`. The first system-instruction chunk carries `tools` and `generation_config`.

**Phase 2 — Content packing:**
After system_instruction is delivered (or if it fit without splitting), pack remaining content items greedily:
1. Start a new chunk with the current envelope
2. For each content item, compute `serialize(chunk + item)`
3. If ≤ limit — add the item to the chunk
4. If > limit — finalize current chunk (send it), start a new chunk with the item
5. If a single item alone exceeds the limit → error (`can_split_under_limit` pre-check)

**Invariants:**
- Every serialized chunk body ≤ `proxy_limit`
- Each chunk is as full as possible (greedy)
- System instruction consumed first (empty input), then user content

**Streaming response:** When the original ingress was streaming (`stream: true`), the final response is SSE events with `Content-Type: text/event-stream`. The final `Interaction` is translated to synthetic `StreamEvent` items via `build_response_from_interaction`.

#### Scenario: Greedy chunk packing — all items fit in one chunk
- GIVEN envelope = 2KB, limit = 100KB, content items = [1KB, 2KB, 3KB]
- WHEN greedy packing runs
- THEN all items fit in one chunk (2KB + 1KB + 2KB + 3KB = 8KB ≤ 100KB)
- AND only one egress request is made

#### Scenario: Greedy chunk packing — splits at boundary
- GIVEN envelope = 2KB, limit = 10KB, content items = [4KB, 5KB, 3KB]
- WHEN greedy packing runs (measurement = serialized chunk size, not raw content size)
- THEN chunk 0 contains [4KB] (2KB + 4KB = 6KB ≤ 10KB; adding 5KB → 11KB > 10KB)
- AND chunk 1 contains [5KB, 3KB] (2KB + 5KB + 3KB = 10KB ≤ 10KB)

#### Scenario: System instruction split triggered by full-chunk measurement
- GIVEN envelope_first = 86KB (tools + gen_config), system_instruction = 27KB, limit = 100KB
- WHEN Phase 1 measures `serialize(envelope_first + system_instruction + empty_input)` = 113KB > 100KB
- THEN `split_text_for_limit` splits system_instruction into parts ≤ (100KB - 86KB - overhead)
- AND each part is sent as a separate chunk with empty input

#### Scenario: Streaming response from split-send (Anthropic)
- GIVEN `stream: true` and request exceeding proxy_limit
- WHEN `handle_split_send` completes all chunks
- THEN response is `Content-Type: text/event-stream` with SSE events synthesized from final `Interaction`

#### Scenario: Non-streaming split-send unchanged
- GIVEN `stream: false` and request exceeding proxy_limit
- WHEN `handle_split_send` completes all chunks
- THEN response is `Content-Type: application/json` (unchanged behavior)

## ADDED

### Requirement: Greedy Chunk Packer

`pack_content_into_chunks(envelope_size, contents, limit) -> Result<Vec<Vec<Content>>, String>` packs content items greedily by full serialized chunk size:

- `envelope_size` is the serialized size of the chunk's non-content fields
- Each content item's contribution is measured by `serialize(chunk_with_item) - serialize(chunk_without_item)`
- Items are added while `serialize(current_chunk + next_item) ≤ limit`
- Returns error if any single item alone exceeds the limit

#### Scenario: Greedy packing fills to limit
- GIVEN envelope = 2KB, limit = 10KB, items = [3KB, 5KB, 4KB]
- WHEN packed greedily
- THEN chunk 0 = [3KB, 5KB] (total 10KB), chunk 1 = [4KB]

#### Scenario: Single item too large rejected
- GIVEN envelope = 2KB, limit = 10KB, items = [12KB]
- WHEN packed greedily
- THEN error returned (single item > limit even in empty chunk)

### Requirement: Synthetic SSE Events from Interaction

When a non-streaming `Interaction` response must be delivered as SSE (split-send path with `stream: true`), the proxy synthesizes `StreamEvent` items from the typed response struct returned by `build_response_from_interaction`:

**Anthropic protocol:**
1. `MessageStart { message: { id, model, role: "assistant" } }`
2. For each `ContentBlock`:
   - `ContentBlockStart { index, content_block }`
   - `ContentBlockDelta { index, delta }` (text_delta or input_json_delta)
   - `ContentBlockStop { index }`
3. `MessageDelta { delta: { stop_reason }, usage }`
4. `MessageStop`

**OpenAI protocol:**
Piped through `ReverseStreamingTranslator` → `ChatCompletionChunk` SSE + `data: [DONE]`.

#### Scenario: Text response synthesized to SSE
- GIVEN `Interaction` with `ModelOutputStep` text "Hello"
- WHEN synthesized to Anthropic SSE
- THEN stream: `message_start` → `content_block_start(text)` → `content_block_delta(text_delta: "Hello")` → `content_block_stop` → `message_delta(end_turn)` → `message_stop`

#### Scenario: Tool use response synthesized to SSE
- GIVEN `Interaction` with `FunctionCallStep`
- WHEN synthesized to Anthropic SSE
- THEN stream includes `content_block_start(tool_use)` → `content_block_delta(input_json_delta)` → `content_block_stop` → `message_delta(tool_use)`

## REMOVED

### Requirement: `split_content_for_limit` (replaced by greedy packer)

The old `split_content_for_limit` measured only content array size, ignoring envelope fields. Replaced by `pack_content_into_chunks` which measures full serialized chunk body size.
