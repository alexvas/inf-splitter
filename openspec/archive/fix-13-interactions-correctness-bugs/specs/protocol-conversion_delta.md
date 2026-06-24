# Delta: Protocol Conversion

**Change ID:** `fix-13-interactions-correctness-bugs`
**Affects:** `src/interactions.rs`, `src/interactions_handler.rs`

---

## ADDED

### Requirement: Anthropic tool_result Content Extraction

`extract_anthropic_content` must handle `tool_result` content blocks in addition to text blocks. When a message's `content` array contains a block with `"type": "tool_result"`, the `content` field of that block (string or array of text blocks) is extracted as text. This ensures tool results reach the upstream model instead of being silently dropped.

#### Scenario: tool_result with string content
- GIVEN Anthropic message content `[{"type":"tool_result","tool_use_id":"tu_1","content":"sunny"}]`
- WHEN `extract_anthropic_content` processes the message
- THEN `Some(Content::TextContent { text: "sunny" })` is returned

#### Scenario: tool_result with array content
- GIVEN Anthropic message content `[{"type":"tool_result","tool_use_id":"tu_1","content":[{"type":"text","text":"result: 42"}]}]`
- WHEN `extract_anthropic_content` processes the message
- THEN text blocks from the array are joined and returned as `TextContent`

#### Scenario: Mixed text and tool_result blocks
- GIVEN content array with both `{"type":"text","text":"a"}` and `{"type":"tool_result","content":"b"}`
- WHEN `extract_anthropic_content` processes the message
- THEN both text and tool_result text are extracted and joined

### Requirement: OpenAI Split-Send SSE Synthesis Includes tool_calls

`synthesize_openai_chunks` must emit `tool_calls` delta chunks when the translated response contains `tool_calls` in the choice message. Without this, clients receive `finish_reason=tool_calls` without the actual tool call data.

#### Scenario: Tool call response synthesized to SSE
- GIVEN `build_response_from_interaction` produces `ChatCompletionResponse` with `choices[0].message.tool_calls = [...]` and `finish_reason = "tool_calls"`
- WHEN `synthesize_openai_chunks` processes the response
- THEN chunks include a `tool_calls` delta chunk with the serialized tool calls
- AND the finish chunk has `finish_reason: "tool_calls"`

#### Scenario: Text-only response unchanged
- GIVEN response has `choices[0].message.content = "hello"` and no `tool_calls`
- WHEN `synthesize_openai_chunks` processes the response
- THEN behavior is unchanged (role, content, finish chunks only)

### Requirement: Exact Retry with Empty interaction_id Returns Error

When `compute_delta` returns an empty delta (`start_index == incoming_count`) and `interaction_id` is empty (not just `None`, but the empty string `""`), the handler must return an error. Sending an empty `ContentList` to the upstream is invalid and produces an opaque upstream error.

#### Scenario: Empty interaction_id on exact retry
- GIVEN session has `{interaction_id: "", message_count: 5}`
- AND client sends 5 messages (same as stored count)
- WHEN handler computes delta and finds `start_index == incoming_count` with `prev_id == None`
- THEN `AppError::Internal` is returned ("session has no interaction_id for replay")
- AND no upstream request is sent

### Requirement: Session Update Failures Are Propagated

When `session_store.update` fails after a successful upstream interaction, the handler must log at error level and return an error to the client. Silently ignoring update failures means the next request uses stale state, causing message duplication or lost continuity.

#### Scenario: Update failure after successful upstream
- GIVEN upstream returns a valid `Interaction`
- WHEN `session_store.update` returns `Err`
- THEN `tracing::error!("session update failed after successful upstream interaction")` is logged
- AND `AppError::Internal` is returned to the client

### Requirement: drop_fields Applied to Interactions Egress

`drop_fields` from route config must be applied to interactions egress requests, matching the behavior of passthrough and conversion paths (OpenAI/Anthropic handlers).

#### Scenario: drop_fields on interactions request
- GIVEN route has `drop_fields = ["thinking"]`
- AND an Anthropic ingress request includes `"thinking": {...}`
- WHEN the interactions handler builds the egress body
- THEN serialized `CreateModelInteractionParams` does not contain `thinking` equivalent fields
- AND valid fields are unaffected

### Requirement: Upstream Response Headers Forwarded Through Interactions Success

Interactions success responses must forward upstream rate-limit and trace headers to the client, matching the whitelist patterns used by passthrough paths (`copy_response_headers` for Anthropic, `relay_response_headers` for OpenAI).

#### Scenario: Gemini rate-limit headers forwarded
- GIVEN interactions upstream returns `x-ratelimit-*` headers
- WHEN the handler builds the success response
- THEN rate-limit headers are forwarded to the client

#### Scenario: Gemini trace headers forwarded
- GIVEN interactions upstream returns `x-request-id` header
- WHEN the handler builds the success response
- THEN `x-request-id` is forwarded to the client

### Requirement: Chunk Envelope Uses Real previous_interaction_id for Size Estimation

`handle_split_send` must use the actual `previous_interaction_id` value (from the prior chunk's returned interaction) for size estimation of subsequent chunks, not a hardcoded 36-character placeholder. Real interaction IDs from Gemini can exceed 36 characters.

#### Scenario: Long interaction ID in chunk envelope
- GIVEN first chunk returns interaction with `id` longer than 36 characters
- WHEN `subsequent_envelope` is built for chunk 2+ size estimation
- THEN `previous_interaction_id` in the envelope matches the real ID length
- AND serialized chunk body ≤ `proxy_limit`

### Requirement: Fallback Response Uses Clamped Token Counts

`build_fallback_response` must use `clamp_i64_to_u32` for converting `Interaction.usage` token counts (which are `i64`) to `u32`, instead of `as u32` which silently wraps/truncates.

#### Scenario: Negative token count clamped to zero
- GIVEN upstream returns `total_input_tokens: -1`
- WHEN `build_fallback_response` constructs usage
- THEN `input_tokens` is clamped to `0u32` with `tracing::warn!`

#### Scenario: Overflow token count clamped to u32::MAX
- GIVEN upstream returns `total_input_tokens: 5_000_000_000`
- WHEN `build_fallback_response` constructs usage
- THEN `input_tokens` is clamped to `u32::MAX` with `tracing::warn!`

### Requirement: Control Sentinel Requires Double Consecutive Appearance

`scan_control_messages` must require the control constant to appear **twice consecutively** (`xyzxyz`) in the message text to trigger. A single mention is treated as normal content. This prevents accidental session wipe when sentinel text appears in chat, logs, or error messages.

#### Scenario: Double appearance triggers control
- GIVEN `clean_all_constant = "CLEAN"` 
- AND message text contains `"...CLEANCLEAN..."`
- WHEN `scan_control_messages` processes the message
- THEN `ControlAction::CleanAll` is returned

#### Scenario: Single appearance is no-op
- GIVEN `clean_all_constant = "CLEAN"`
- AND message text contains `"...CLEAN..."`
- WHEN `scan_control_messages` processes the message
- THEN message is NOT stripped
- AND `action` is `None`

#### Scenario: Single appearance of extend lifetime is no-op
- GIVEN `extend_lifetime_constant = "EXTEND<unix_utc>END"`
- AND message text contains `"...EXTEND1718571800END..."` (single)
- WHEN `scan_control_messages` processes the message
- THEN message is NOT stripped

#### Scenario: Double appearance of extend lifetime triggers control
- GIVEN `extend_lifetime_constant = "EXTEND<unix_utc>END"`
- AND message text contains `"...EXTEND1718571800ENDEXTEND1718571800END..."`
- WHEN `scan_control_messages` processes the message
- THEN `ControlAction::ExtendLifetime(1718571800)` is returned

## MODIFIED

### Requirement: Split-Send Session Progress Uses Content Index

Session `message_count` updates during split-send track delivered **Content items by index** instead of estimating from proportional rounding. The final session update uses the total ingress message count.

#### Scenario: Content-indexed progress
- GIVEN 2 ingress messages produce 3 Content items, split into chunks of [2, 1]
- WHEN session is updated after chunk 1
- THEN `message_count` is set to `min(delivered_content_count, total_message_count)` where `delivered_content_count` = 2
- AND after chunk 2, `message_count` is the full `total_message_count` (2 ingress messages)

#### Scenario: No proportional drift on retry
- GIVEN chunk 1 delivered 2 of 3 Content items (from 2 ingress messages)
- WHEN session is queried after chunk 1
- THEN `message_count` is not rounded to 1 (which would skip message 2 on retry)
- INSTEAD `message_count` reflects actual delivered Content items: 2

### Requirement: Streaming Pending Session Recovery After Disconnect Before interaction.created

When a streaming session is marked pending with `interaction_id=""` and the client disconnects before `interaction.created` is parsed, the stream error path must clear the pending flag so startup recovery can remove the orphaned session. Currently the session remains `pending=true` with empty `interaction_id`, making startup verification impossible (cannot probe upstream without an ID).

#### Scenario: Disconnect before interaction.created
- GIVEN streaming session marked `{pending: true, interaction_id: "", message_count: 3}`
- AND client disconnects before `interaction.created` SSE event is parsed
- WHEN the stream task error path runs
- THEN `session_store.update(sid, "", message_count, false)` is called (pending cleared)
- AND startup recovery removes the session (since it has no valid interaction_id)

### Requirement: Header Cross-Mapping in Interactions Egress

`x-request-id` is generated by upstream (OpenAI) and returned in responses. The proxy must save it to session state so subsequent requests can reference it via `previous_interaction_id`. Incoming `x-claude-code-session-id` (from Anthropic ingress) must be forwarded as `X-Client-Request-Id` to OpenAI upstream for client-request correlation.

#### Scenario: Upstream x-request-id saved to session
- GIVEN interactions upstream returns `x-request-id: req-abc-123` in response headers
- WHEN proxy processes the successful response
- THEN `req-abc-123` is saved in session state as the last `x-request-id`
- AND the next request to same session can use it as `previous_interaction_id`

#### Scenario: x-claude-code-session-id forwarded as X-Client-Request-Id
- GIVEN client sends `x-claude-code-session-id: sess-789`
- AND the route targets an OpenAI upstream
- WHEN proxy builds the upstream request
- THEN `X-Client-Request-Id: sess-789` header is included in the upstream request

#### Scenario: x-claude-code-session-id absent — no X-Client-Request-Id
- GIVEN client does NOT send `x-claude-code-session-id`
- WHEN proxy builds the upstream request
- THEN no `X-Client-Request-Id` header is added

## REMOVED

(None)
