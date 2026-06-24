# Delta: Protocol Conversion (Interactions)

**Change ID:** `fix-interactions-session-and-streaming`
**Affects:** `src/interactions_handler.rs`, `src/interactions.rs`, `src/control.rs`, `src/lib.rs`

---

## MODIFIED

### Requirement: Proxy-Limit Split-Send Chunk Forwarding

Updated to fix three correctness bugs in the split-send path.

**Change 1 — System-instruction split responses stored:**

When `send_split_system_instruction` sends system-instruction chunks, each successful chunk's parsed `Interaction` is stored in `last_interaction` so that `build_fallback_response` has data to construct a valid response when no content chunks follow. Previously the system-instruction loop discarded the response, causing an empty fallback.

**Change 2 — Atomic session updates:**

Session state (`message_count`, `previous_interaction_id`) is updated after **each** successful chunk, not only after all chunks complete. This prevents content duplication on retry: if chunk 2 of 3 fails after chunk 1 was accepted upstream, the retry starts from chunk 2 with the correct `previous_interaction_id` instead of re-sending chunk 1.

**Change 3 — Chunk size estimation includes previous_interaction_id:**

`pack_content_into_chunks` now includes `previous_interaction_id` in the serialized envelope used for size measurement of subsequent chunks, matching what `build_chunk_request` actually serializes. Previously the measurement omitted this field, so actual chunk bodies could exceed `proxy_limit`.

#### Scenario: System-instruction-only split returns valid response
- GIVEN `proxy_limit` is low enough to split `system_instruction` but high enough that no content chunking is needed
- AND all content fits in one chunk after system-instruction chunks
- WHEN `handle_split_send` processes the request
- THEN the system-instruction chunk response is stored in `last_interaction`
- AND the content chunk's response replaces `last_interaction`
- AND the final response is built from the content chunk's `Interaction` (not an empty fallback)

#### Scenario: Split-send retry after chunk failure does not duplicate
- GIVEN a 3-chunk split-send where chunk 1 succeeds upstream
- AND chunk 2 fails with a network error
- WHEN the caller retries the same request
- THEN `message_count` reflects chunk 1's accepted messages
- AND `previous_interaction_id` is chunk 1's interaction ID
- AND the retry starts sending from chunk 2 (no duplication of chunk 1)

#### Scenario: Chunk size estimation matches actual serialized size
- GIVEN `proxy_limit = "100k"` and a follow-up interaction with `previous_interaction_id = "abc123..."`
- WHEN `pack_content_into_chunks` measures subsequent chunk sizes
- THEN the envelope used for measurement includes `previous_interaction_id`
- AND the measured size matches what `serde_json::to_vec(&chunk_request)` produces
- AND no chunk silently exceeds `proxy_limit`

---

### Requirement: Interactions → Anthropic Translation (Streaming)

Updated to fix two streaming event translation bugs.

**Change 1 — No duplicate content_block_start for index 0:**

`InteractionCreatedEvent` no longer unconditionally emits `content_block_start(0)`. If a subsequent `StepStart` event also targets index 0, the `InteractionCreatedEvent` start is suppressed (or the `StepStart` deduplicates). The proxy tracks the active content block index and skips duplicate starts.

**Change 2 — ContentBlockStop uses correct index:**

`InteractionCompletedEvent` emits `ContentBlockStop` for the **last active** block index (tracked from `StepStart`/`StepStop` events), not hardcoded index 0. For multi-step or tool-use streams where `StepStop` already closed a non-zero index, the completion event closes the correct remaining block.

#### Scenario: No duplicate content_block_start for index 0
- GIVEN an interactions SSE stream with `interaction.created` then `step.start { index: 0, type: "model_output" }`
- WHEN the proxy translates the stream to Anthropic format
- THEN only one `content_block_start` with index 0 is emitted
- AND the client receives a single `ContentBlockStart` for index 0

#### Scenario: ContentBlockStop uses last active block index
- GIVEN a multi-step stream with `step.start { index: 1 }` → `step.stop { index: 1 }` → `interaction.completed`
- WHEN the proxy translates `interaction.completed`
- THEN `ContentBlockStop` is emitted for index 1 (the last active block)
- AND no duplicate or wrong-index `ContentBlockStop` is emitted

#### Scenario: Tool-use stream stop index
- GIVEN a stream with `step.start { index: 2, type: "function_call" }` → `step.delta` → `interaction.completed` (no `step.stop` before completion)
- WHEN the proxy translates `interaction.completed`
- THEN `ContentBlockStop` is emitted for index 2
- AND the `stop_reason` reflects `"tool_use"` from the interaction status

---

### Requirement: Interactions Streaming Events (Error Handling)

Updated streaming translation event categories.

| Category | Example | Behavior | Rationale |
|----------|---------|----------|-----------|
| **Malformed data** | Invalid JSON, unknown `event_type` | `tracing::info!` with raw data prefix (first 200 chars), then drop | `info!` (not `warn!`) because protocol evolution may introduce new event types that are safe to ignore |
| **No client impact** | `interaction.status_update` | Skip silently; code comment explains why | `tracing::warn!` would produce noise for an expected, harmless event |
| **Not yet implemented** | Unhandled delta types | `tracing::warn!` with event type; stream continues | Makes the gap visible to operators |
| **Duplicate block start** | `content_block_start` for already-active index | Skip silently; code comment explains deduplication | The `InteractionCreatedEvent` may imply a block start that a subsequent `StepStart` makes explicit |

---

### Requirement: Interactions Schema Patching (No Change)

No change to schema patching. Existing behavior preserved.

---

### Requirement: translate_stream_event Uses Typed InteractionSseEvent (No Change)

No change to typed event dispatch. Existing behavior preserved.

---

## ADDED

### Requirement: max_tokens Clamping at u32::MAX

When constructing `GenerationConfig` from ingress `max_tokens`, values above `u32::MAX` are clamped to `u32::MAX` with a `tracing::warn!` log, instead of silently truncating via `as u32` (which wraps).

#### Scenario: max_tokens above u32::MAX clamped
- GIVEN a client sends `max_tokens: 5000000000` (exceeds u32::MAX)
- WHEN the interactions handler constructs `GenerationConfig`
- THEN `max_output_tokens` is set to `u32::MAX` (4294967295)
- AND `tracing::warn!("max_tokens {} exceeds u32::MAX, clamping", 5000000000_u64)` is emitted

#### Scenario: max_tokens within range unchanged
- GIVEN a client sends `max_tokens: 4096`
- WHEN the interactions handler constructs `GenerationConfig`
- THEN `max_output_tokens` is set to `4096` (unchanged)

---

### Requirement: extend_lifetime Matches Timestamp at End of Message

`scan_control_messages` in `control.rs` correctly extracts the timestamp from `extend_lifetime` control messages even when the timestamp is the final token in the message (no trailing non-digit character after the digits).

#### Scenario: Timestamp at end of message
- GIVEN a control template `"extend "` and a message `"extend 1718571800"`
- AND `1718571800` is at the end of the message with no characters after it
- WHEN `scan_control_messages` processes the message
- THEN `after_prefix.find(|c: char| !c.is_ascii_digit())` returns `None`
- AND the handler treats the entire remaining string `"1718571800"` as the timestamp
- AND the session lifetime is extended to Unix timestamp 1718571800

#### Scenario: Timestamp not at end (existing behavior preserved)
- GIVEN a message `"extend 1718571800 some trailing text"`
- WHEN `scan_control_messages` processes the message
- THEN the timestamp `1718571800` is correctly extracted as before

---

## REMOVED

(None)
