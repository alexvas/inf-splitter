# Spec: Protocol Conversion

Components: `src/openai.rs`, `src/anthropic.rs`, `src/sse.rs`, `src/relay.rs`, `src/interactions.rs`, `src/interactions_handler.rs`, `src/interactions_types.rs`

## Requirement: OpenAI→Anthropic Translation

When OpenAI-format ingress must reach an Anthropic-compatible upstream, `AnthropicHandler` converts:
- `ChatCompletionRequest` → `MessageCreateRequest` via `anyllm_translate`
- Request body is parsed, translated, serialized, and sent upstream
- Response is translated back from Anthropic to OpenAI format

### Scenario: Non-streaming conversion
- GIVEN `POST /v1/chat/completions` with `"stream": false`
- WHEN routing to an Anthropic-only section
- THEN the full Anthropic response is collected and translated back to OpenAI chat completion format

### Scenario: Streaming conversion
- GIVEN `POST /v1/chat/completions` with `"stream": true`
- WHEN routing to an Anthropic-only section
- THEN each SSE event from the upstream is translated to OpenAI streaming format and streamed back

## Requirement: Anthropic→OpenAI Translation

When Anthropic-format ingress must reach an OpenAI-compatible upstream, `OpenAiHandler` converts:
- `MessageCreateRequest` → `ChatCompletionRequest` via `anyllm_translate`
- Request body is parsed, translated, serialized, and sent upstream
- Response is translated back from OpenAI to Anthropic format

Before translation, `strip_adaptive_thinking()` removes the `thinking` field when its type is `"adaptive"` — unsupported by `anyllm_translate` 0.9.x `ThinkingConfig`. Since the translation doesn't propagate thinking blocks anyway, removal is safe.

### Scenario: Non-streaming conversion
- GIVEN `POST /v1/messages` with `"stream": false`
- WHEN routing to an OpenAI-only section
- THEN the full OpenAI response is collected and translated back to Anthropic message format

### Scenario: Streaming conversion
- GIVEN `POST /v1/messages` with `"stream": true`
- WHEN routing to an OpenAI-only section
- THEN each SSE event from the upstream is translated to Anthropic streaming format and streamed back

## Requirement: Token Limit Injection

Token limits (`max_tokens`, `max_output_tokens`, `max_completion_tokens`) are injected into outgoing requests:
- **Passthrough paths:** JSON body is parsed, `cap_numeric_field()` clamps or sets the field
- **Conversion paths:** typed request structs are mutated before serialization
- `max_output_tokens` only applies via JSON passthrough (Anthropic struct has only `max_tokens`)

### Scenario: Passthrough token cap
- GIVEN config has `max_tokens = 100` and client sends `max_tokens: 500`
- WHEN request is sent upstream via passthrough
- THEN the outgoing body has `max_tokens: 100`

### Scenario: Conversion token cap
- GIVEN config has `max_tokens = 100` and client sends `max_tokens: 500`
- WHEN request is translated and sent upstream
- THEN the outgoing `MessageCreateRequest.max_tokens` is `100`

## Requirement: stream_options Handling

`stream_options` is always dropped from OpenAI requests before forwarding upstream. This is hardcoded.

### Scenario: stream_options stripped
- GIVEN a request body includes `"stream_options": {...}`
- WHEN the body is forwarded to an OpenAI upstream
- THEN `stream_options` is removed from the outgoing body

## Requirement: SSE Utilities

`src/sse.rs` provides shared utilities for detecting and handling Server-Sent Events:
- Detection of `text/event-stream` content type
- SSE line parsing
- Event formatting for downstream streaming responses

### Scenario: SSE content type detection
- GIVEN an upstream response with `Content-Type: text/event-stream`
- WHEN the handler receives the response
- THEN it enters streaming mode, parsing SSE events line by line

## Requirement: Diagnostic Relay Stream

`DiagnosticStream` wraps upstream SSE streams to buffer response data for diagnostics dumps:
- Buffers up to `MAX_STREAMING_DUMP_BYTES`
- On stream termination, records the buffered body as a dump event
- Drops buffering once a dump has been recorded for the stream

### Scenario: Streaming response dump
- GIVEN `dump_mode = "all"` and a streaming request
- WHEN the SSE stream completes
- THEN the accumulated body is recorded as an egress response dump

## Requirement: Drop Fields on Egress

`drop_fields` are applied to outgoing request bodies on all four routing paths. The shared helper `apply_egress_transforms` handles passthrough paths (token caps + field drops on parsed `Value`). The helper `prepare_egress_body` handles conversion paths (serialize struct → Value → drop → bytes).

### Scenario: Drop on passthrough
- GIVEN `drop_fields = ["user"]` on a passthrough section
- WHEN a request with `"user":"abc"` is processed
- THEN the upstream receives the body without the `user` field

### Scenario: Drop on conversion
- GIVEN `drop_fields = ["logprobs"]` and an OpenAI→Anthropic conversion section
- WHEN an OpenAI ingress request includes `"logprobs": true`
- THEN the translated Anthropic request does not contain `logprobs`

## Requirement: apply_egress_transforms

`lib.rs` provides `apply_egress_transforms(value, model, route)` for passthrough paths — applies token caps via `apply_token_caps_to_value`, resolves `drop_fields` for the model, and removes them via `drop_fields_from_value`. Used by both `OpenAiHandler` and `AnthropicHandler` passthrough.

## Requirement: prepare_egress_body

`lib.rs` provides `prepare_egress_body(req, model, route, diagnostics)` for conversion paths — serializes a typed request struct to `serde_json::Value`, applies `drop_fields`, serializes to bytes, and returns `PreparedBody { bytes, value, egress_str }`. Used by all four conversion paths (both streaming and non-streaming in both files).

## Requirement: Anthropic Response Header Whitelist

`copy_response_headers()` in `anthropic.rs` forwards only these response headers from upstream to client:
- `content-type`
- `request-id`
- `x-request-id`
- `x-claude-code-session-id`
- `anthropic-ratelimit-requests-limit`
- `anthropic-ratelimit-requests-remaining`
- `anthropic-ratelimit-requests-reset`
- `anthropic-ratelimit-tokens-limit`
- `anthropic-ratelimit-tokens-remaining`
- `anthropic-ratelimit-tokens-reset`

All other upstream response headers are filtered out. Additionally, if `x-claude-code-session-id` is present and `x-request-id` is absent, `x-request-id` is inserted with the same value — so OpenAI clients get their expected header when the upstream is Anthropic.

The OpenAI response relay path applies its own header forwarding via `relay_response_headers`.

### Scenario: Anthropic upstream → OpenAI client gets x-request-id
- GIVEN Anthropic upstream response has `x-claude-code-session-id: sess-1`
- WHEN `copy_response_headers` processes the response
- THEN both `x-claude-code-session-id: sess-1` and `x-request-id: sess-1` are relayed

## Requirement: OpenAI Response Header Whitelist

`relay_response_headers()` in `openai.rs` forwards these response headers from upstream to client:

- `content-type`
- `x-ratelimit-*` (all rate-limit headers)
- `x-request-id`
- `request-id`
- `x-claude-code-session-id`
- `openai-*` (all OpenAI-specific headers)

Additionally, if `x-request-id` (or `request-id`) is present and `x-claude-code-session-id` is absent, `x-claude-code-session-id` is inserted with the same value — so Anthropic (Claude CLI) clients get their expected header when the upstream is OpenAI.

### Scenario: OpenAI upstream → Anthropic client gets x-claude-code-session-id
- GIVEN OpenAI upstream response has `x-request-id: req-abc`
- WHEN `relay_response_headers` processes the response
- THEN both `x-request-id: req-abc` and `x-claude-code-session-id: req-abc` are relayed

## Requirement: Anthropic → Interactions Translation

`InteractionsHandler` converts Anthropic ingress to `CreateModelInteractionParams`:

- Ingress body parsed at boundary — `model`, `stream`, `temperature`, `max_tokens` extracted as typed scalars
- `messages[]` → interactions `Content[]` via typed extractors
- `system` → `system_instruction` — **only on the first interaction** (when `previous_interaction_id` is `None`)
- `max_tokens` → `generation_config.max_output_tokens` — **only on the first interaction**
- `temperature` → `generation_config.temperature` — **only on the first interaction**
- `tools` and `tool_choice` extracted from ingress body — **only on the first interaction** (when `previous_interaction_id` is `None`)
- `previous_interaction_id` set from session state (if exists)
- All parameters passed as typed scalars to `build_interactions_request_anthropic`, which returns `CreateModelInteractionParams` directly

Only messages not yet delivered to the session are included (delta computation). Control messages are stripped before construction. `system_instruction`, `tools`, `tool_choice`, and `generation_config` are omitted on follow-up interactions — the upstream reuses the interaction's existing configuration.

### Scenario: First request in session
- GIVEN no prior session state
- WHEN Anthropic request with 3 messages arrives
- THEN all 3 messages are translated, no `previous_interaction_id` sent
- AND `system_instruction`, `tools`, and `generation_config` are included in the outgoing request

### Scenario: Subsequent request — delta + chain
- GIVEN session has `{interaction_id: "abc123", delivered_count: 3}`
- WHEN Anthropic request with 5 messages arrives (same session)
- THEN only messages [3..5] are sent, `previous_interaction_id: "abc123"` is set
- AND `system_instruction`, `tools`, and `generation_config` are **absent** (interaction reuses existing config)

### Scenario: Subsequent request — no new messages
- GIVEN session has `{interaction_id: "abc123", delivered_count: 3}`
- WHEN Anthropic request with 3 messages arrives (same 3 messages, no new content)
- THEN `compute_delta(3, 3)` returns `(3, 3)` — an empty slice, no messages sent

### Scenario: Context reset — fewer messages than delivered
- GIVEN session has `{interaction_id: "abc123", delivered_count: 5}`
- WHEN Anthropic request with 2 messages arrives (client started new conversation)
- THEN `compute_delta(5, 2)` returns `(0, 2)` — re-send all 2 messages
- AND `system_instruction`, `tools`, and `generation_config` are included (new interaction, `previous_interaction_id` is `None`)

### Scenario: System instruction split — chunks correctly chained
- GIVEN a large `system_instruction` that exceeds `proxy_limit`
- WHEN `send_split_system_instruction` splits the text and sends chunks
- THEN chunk 1 has `system_instruction = part[0]`, no `previous_interaction_id`
- AND chunk N has `system_instruction = part[N]`, `previous_interaction_id = chunk_N-1.id`
- AND the split path uses `build_chunk_request` (not `build_request_body`), so the `is_first` guard does not apply

### Scenario: Tools forwarded to interactions API
- GIVEN Anthropic ingress body with `"tools": [{"name": "get_weather", "input_schema": {...}}]` and `"tool_choice": {"type": "auto"}`
- WHEN the interactions request is built
- THEN `CreateModelInteractionParams.tools` contains the tool definitions as `Vec<Tool>`
- AND `generation_config.tool_choice` reflects the tool choice (as `ToolChoice::Simple("auto")`, serialized to JSON)

## Requirement: Interactions → Anthropic Translation

`Interaction` response translates to Anthropic `MessageResponse`:
- `Interaction.steps[]` → Anthropic `content[]` blocks: text from `ModelOutputStep`, tool_use from `FunctionCallStep`
- `Interaction.status` → `stop_reason`: `"end_turn"` for `"completed"`, `"tool_use"` for `"requires_action"`
- `Interaction.usage` → response usage metadata
- Non-streaming response construction uses shared `build_response_from_interaction()` across all three paths (`send_and_translate`, `handle_split_send`, `send_split_system_instruction`)
- Stream: `step.*` events → Anthropic `StreamEvent` SSE, including function_call → tool_use blocks
- Stream: `InteractionCompletedEvent` → final events with appropriate stop_reason

### Scenario: Text response from interactions
- GIVEN `Interaction` with `ModelOutputStep` containing text
- WHEN translated to Anthropic format
- THEN response has `{"type": "message", "role": "assistant", "content": [{"type": "text", "text": "..."}], "stop_reason": "end_turn"}`

### Scenario: Function call response from interactions (non-streaming)
- GIVEN `Interaction` with `status: "requires_action"` and `FunctionCallStep { id: "call-1", name: "get_weather", arguments: {"location": "Boston"} }`
- WHEN translated to Anthropic format
- THEN `content` contains `ContentBlock::ToolUse { id: "call-1", name: "get_weather", input: {"location": "Boston"} }`
- AND `stop_reason` is `"tool_use"`

## Requirement: OpenAI → Interactions Translation

OpenAI ingress → `CreateModelInteractionParams`:
- Ingress body parsed at boundary — `model`, `stream`, `temperature`, `max_tokens` extracted as typed scalars
- `messages[]` → interactions `Content[]` via typed extractors
- System message (role=system) → `system_instruction` — **only on the first interaction**
- `max_tokens` → `generation_config.max_output_tokens` — **only on the first interaction**
- `temperature` → `generation_config.temperature` — **only on the first interaction**
- `tools` and `tool_choice` extracted from ingress body — **only on the first interaction**
- All parameters passed as typed scalars to `build_interactions_request_openai`, which returns `CreateModelInteractionParams` directly

`system_instruction`, `tools`, `tool_choice`, and `generation_config` are omitted on follow-up interactions.

### Scenario: First request — tools and system_instruction present
- GIVEN no prior session state
- WHEN OpenAI request with `tools` and a system message arrives
- THEN `system_instruction`, `tools`, and `generation_config` are included in the outgoing `CreateModelInteractionParams`

### Scenario: Subsequent request — tools and system_instruction absent
- GIVEN session has `{interaction_id: "abc123", delivered_count: 2}`
- WHEN OpenAI request with same tools and system message arrives (same session)
- THEN only new messages are sent (delta)
- AND `system_instruction`, `tools`, and `generation_config` are **absent** from the outgoing request

### Scenario: OpenAI tools forwarded to interactions API
- GIVEN OpenAI ingress body with `"tools": [{"type": "function", "function": {"name": "search"}}]` and `"tool_choice": "auto"`
- WHEN the interactions request is built
- THEN `CreateModelInteractionParams.tools` contains the tool definitions
- AND `generation_config.tool_choice` reflects the tool choice

## Requirement: Proxy-Limit Split-Send Chunk Forwarding

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

### Scenario: First chunk carries tools and generation_config
- GIVEN a request with `tools` and `generation_config` that exceeds `proxy_limit`
- WHEN `handle_split_send` builds the first chunk (`current_prev` is `None`)
- THEN the chunk request includes `tools` and `generation_config`
- AND `system_instruction` is included on the first chunk

### Scenario: Subsequent chunks omit tools and generation_config
- GIVEN a split-send sequence where the first chunk already created the interaction
- WHEN `handle_split_send` builds chunk 2+ (`current_prev` is `Some(...)`)
- THEN `tools` and `generation_config` are `None`
- AND `system_instruction` is `None`

### Scenario: System instruction split first chunk carries tools
- GIVEN a request where both content and system_instruction need splitting
- WHEN `send_split_system_instruction` builds the first system-instruction chunk
- THEN the chunk includes `tools` and `generation_config`

### Scenario: Greedy chunk packing — all items fit in one chunk
- GIVEN envelope = 2KB, limit = 100KB, content items = [1KB, 2KB, 3KB]
- WHEN greedy packing runs
- THEN all items fit in one chunk (2KB + 1KB + 2KB + 3KB = 8KB ≤ 100KB)
- AND only one egress request is made

### Scenario: Greedy chunk packing — splits at boundary
- GIVEN envelope = 2KB, limit = 10KB, content items = [4KB, 5KB, 3KB]
- WHEN greedy packing runs (measurement = serialized chunk size, not raw content size)
- THEN chunk 0 contains [4KB] (2KB + 4KB = 6KB ≤ 10KB; adding 5KB → 11KB > 10KB)
- AND chunk 1 contains [5KB, 3KB] (2KB + 5KB + 3KB = 10KB ≤ 10KB)

### Scenario: System instruction split triggered by full-chunk measurement
- GIVEN envelope_first = 86KB (tools + gen_config), system_instruction = 27KB, limit = 100KB
- WHEN Phase 1 measures `serialize(envelope_first + system_instruction + empty_input)` = 113KB > 100KB
- THEN `split_text_for_limit` splits system_instruction into parts ≤ (100KB - 86KB - overhead)
- AND each part is sent as a separate chunk with empty input

### Scenario: Streaming response from split-send (Anthropic)
- GIVEN `stream: true` and request exceeding proxy_limit
- WHEN `handle_split_send` completes all chunks
- THEN response is `Content-Type: text/event-stream` with SSE events synthesized from final `Interaction`

### Scenario: Non-streaming split-send unchanged
- GIVEN `stream: false` and request exceeding proxy_limit
- WHEN `handle_split_send` completes all chunks
- THEN response is `Content-Type: application/json` (unchanged behavior)

## Requirement: Greedy Chunk Packer

`pack_content_into_chunks(first_envelope, subsequent_envelope, contents, limit) -> Result<Vec<Vec<Content>>, String>` packs content items greedily by full serialized chunk body size:

- `first_envelope` is a `CreateModelInteractionParams` template for the first chunk (with tools, generation_config, system_instruction)
- `subsequent_envelope` is the template for all following chunks (with `previous_interaction_id`, without first-only fields)
- Each content item is added while `serialize(envelope + current_items + item) ≤ limit`
- Returns error if any single item alone exceeds the limit in an otherwise-empty chunk

### Scenario: Greedy packing fills to limit
- GIVEN envelope = 2KB, limit = 10KB, items = [3KB, 5KB, 4KB]
- WHEN packed greedily
- THEN chunk 0 = [3KB, 5KB] (total 10KB), chunk 1 = [4KB]

### Scenario: Single item too large rejected
- GIVEN envelope = 2KB, limit = 10KB, items = [12KB]
- WHEN packed greedily
- THEN error returned (single item > limit even in empty chunk)

## Requirement: Synthetic SSE Events from Interaction

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
Constructed via `openai_sse_role_chunk`, `openai_sse_content_chunk`, `openai_sse_finish_chunk` factory functions → `ChatCompletionChunk` SSE + `data: [DONE]`.

### Scenario: Text response synthesized to SSE
- GIVEN `Interaction` with `ModelOutputStep` text "Hello"
- WHEN synthesized to Anthropic SSE
- THEN stream: `message_start` → `content_block_start(text)` → `content_block_delta(text_delta: "Hello")` → `content_block_stop` → `message_delta(end_turn)` → `message_stop`

### Scenario: Tool use response synthesized to SSE
- GIVEN `Interaction` with `FunctionCallStep`
- WHEN synthesized to Anthropic SSE
- THEN stream includes `content_block_start(tool_use)` → `content_block_delta(input_json_delta)` → `content_block_stop` → `message_delta(tool_use)`

## Requirement: Interactions → OpenAI Translation

`Interaction` → OpenAI `ChatCompletionResponse`:
- `Interaction.steps[]` → `choices[].message`: text from `ModelOutputStep` → `content`, `FunctionCallStep` → `tool_calls`
- `Interaction.status` → `finish_reason`: `"stop"` for `"completed"`, `"tool_calls"` for `"requires_action"`
- Non-streaming response construction uses shared `build_response_from_interaction()`
- Stream: `step.*` events → OpenAI streaming chunks via `ReverseStreamingTranslator`, `[DONE]` on completion
- Stream: function_call `step.start`/`step.delta` events flow through `ReverseStreamingTranslator` to produce OpenAI-format `tool_calls` chunks

## Requirement: Interactions Streaming Events

Streaming from interactions endpoint returns SSE with discriminated event types:

| Event type | Meaning |
|-----------|---------|
| `interaction.created` | Interaction created, contains full initial state |
| `interaction.status_update` | Status change (skipped in translation) |
| `step.start` | A new step begins (thought, model_output, tool_call, etc.) |
| `step.delta` | Incremental output for the current step (text, thought_signature, arguments_delta for function_call, etc.) |
| `step.stop` | Current step completes, includes per-step usage |
| `error` | Stream-level error |
| `interaction.completed` | Final interaction with total usage |

> **Note:** The old `content.delta` / `ContentDelta` events are no longer emitted by the current Gemini Interactions API. The protocol now uses `step.*` events.

### Scenario: Streaming Anthropic ingress → Interactions upstream
- GIVEN `POST /v1/messages` with `"stream": true`
- WHEN routing to interactions endpoint with `stream: true`
- THEN SSE events translated to Anthropic SSE format:
  - `interaction.created` → `message_start` + `content_block_start` (initial text block)
  - `step.start` → `content_block_start`
  - `step.delta` (text) → `content_block_delta { type: "text_delta" }`
  - `step.delta` (thought_signature) → `content_block_delta { type: "signature_delta" }`
  - `step.stop` → `content_block_stop`
  - `error` → `event: error` with `{"type": "error", "error": {"type": "<code>", "message": "<message>"}}`
  - `interaction.completed` → `message_delta { stop_reason }` + `message_stop`

### Scenario: Streaming OpenAI ingress → Interactions upstream
- GIVEN `POST /v1/chat/completions` with `"stream": true`
- WHEN routing to interactions endpoint with `stream: true`
- THEN step.* events translated to OpenAI SSE chunks, `[DONE]` on completion (via `ReverseStreamingTranslator`)

### Scenario: Full stream lifecycle with thinking
- GIVEN an interactions response with:
  - `interaction.created`
  - `step.start { type: "thought" }`
  - `step.delta { delta: { type: "thought_signature", signature: "..." } }`
  - `step.stop`
  - `step.start { type: "model_output" }`
  - `step.delta { delta: { type: "text", text: "Hello" } }`
  - `step.stop`
  - `interaction.completed`
- WHEN the proxy translates the stream to Anthropic format
- THEN the client receives:
  - `message_start`
  - `content_block_start` (text type, from interaction.created)
  - `content_block_start` (text type, from step.start thought)
  - `content_block_delta { type: "signature_delta", signature: "..." }`
  - `content_block_stop`
  - `content_block_start` (text type, from step.start model_output)
  - `content_block_delta { type: "text_delta", text: "Hello" }`
  - `content_block_stop`
  - `message_delta { stop_reason: "end_turn" }`
  - `message_stop`

### Scenario: Error event translation
- GIVEN an interactions SSE event `{"event_type": "error", "error": {"code": "not_found", "message": "Result not found."}}`
- WHEN the proxy translates it to Anthropic streaming format
- THEN the client receives `event: error` with `{"type": "error", "error": {"type": "not_found", "message": "Result not found."}}`

### Scenario: Streaming function_call step.start
- GIVEN SSE event `{"event_type":"step.start","index":2,"step":{"type":"function_call","id":"call-1","name":"get_weather"}}`
- WHEN translated
- THEN emits `content_block_start` with `index: 2`, `content_block: {type: "tool_use", id: "call-1", name: "get_weather", input: {}}`

### Scenario: Streaming function_call step.delta (arguments_delta)
- GIVEN SSE event `{"event_type":"step.delta","index":2,"delta":{"type":"arguments_delta","arguments":"{\"location\":\"Boston\"}"}}`
- WHEN translated
- THEN emits `content_block_delta` with `index: 2`, `delta: {type: "input_json_delta", partial_json: "{\"location\":\"Boston\"}"}`

### Scenario: Full stream lifecycle with function_call
- GIVEN an interactions response with:
  - `interaction.created`
  - `step.start { type: "thought" }` → thought_signature deltas → `step.stop`
  - `step.start { type: "function_call", id: "call-1", name: "get_weather" }`
  - `step.delta { delta: { type: "arguments_delta", arguments: "{\"location\":\"Boston\"}" } }`
  - `step.stop`
  - `interaction.completed`
- WHEN the proxy translates the stream to Anthropic format
- THEN the client receives:
  - `message_start`
  - `content_block_start` (text type, from interaction.created)
  - `content_block_start` (text type, from step.start thought)
  - `content_block_delta { type: "signature_delta" }`
  - `content_block_stop`
  - `content_block_start { content_block: { type: "tool_use", id: "call-1", name: "get_weather", input: {} } }`
  - `content_block_delta { delta: { type: "input_json_delta", partial_json: "{\"location\":\"Boston\"}" } }`
  - `content_block_stop`
  - `message_delta` + `message_stop`

## Requirement: Interactions Schema Patching

`build.rs` patches the OpenAPI schema before code generation to match actual Gemini API behavior:

| Schema | Field | Patch | Reason |
|--------|-------|-------|--------|
| `Interaction` | `created` | Removed from required | SSE `interaction.created` events have incomplete initial state |
| `Interaction` | `updated` | Removed from required | Same as above |
| `Interaction` | `steps` | Removed from required | Same as above |
| `FunctionCallStep` | `arguments` | Removed from required | SSE `step.start` events may not include arguments initially |

### Scenario: FunctionCallStep deserializes without arguments
- GIVEN a `step.start` SSE event with `{"type":"function_call","id":"call-1","name":"get_weather"}` (no `arguments`)
- WHEN deserialized as `Step::FunctionCallStep`
- THEN `arguments` is `serde_json::Value::Null` (via `#[serde(default)]`)
- AND the step.start event translates to a `tool_use` `content_block_start` with empty `input: {}`

Rust types for the interactions protocol are generated at build time from `schemas/interactions.openapi.json` by `build.rs`. The generated code is included in `src/interactions_types.rs` via `include!`.

The `Interaction` schema's `required` array is patched in `build.rs` to remove `created`, `updated`, and `steps` — the Gemini API does not consistently include these fields in SSE event payloads (specifically in `interaction.created` events where the interaction is still in-progress). Code accessing these fields must handle them as `Option<T>`.

### Scenario: Schema is committed
- GIVEN `schemas/interactions.openapi.json` exists in the repo
- WHEN `cargo build` runs
- THEN build.rs generates types without network access

### Scenario: Schema patching removes required fields
- GIVEN `schemas/interactions.openapi.json` has `Interaction.required: ["created", "id", "status", "steps", "updated"]`
- WHEN `build.rs` runs
- THEN the in-memory schema is patched so `Interaction.required` is `["id", "status"]`
- AND the generated `Interaction` struct has `created: Option<String>`, `updated: Option<String>`, `steps: Option<Vec<Step>>`

### Scenario: interaction.created SSE event deserializes
- GIVEN SSE data `{"event_type":"interaction.created","interaction":{"id":"abc","status":"in_progress","model":"gemini-3.1-flash-lite"}}`
- WHEN `serde_json::from_str::<InteractionSseEvent>(data)` is called
- THEN it succeeds as `InteractionSseEvent::InteractionCreatedEvent`
- AND the inner `Interaction` has `created: None`, `steps: None`, `updated: None`

## Requirement: build.rs Infers OneOf Discriminator Tags from const

When a oneOf discriminator schema has no explicit `mapping`, `build.rs` must inspect each variant's target schema for a `const` value on the discriminator property. If found, this value becomes the `#[serde(rename)]` tag for the enum variant.

### Scenario: Tag inferred from const on event_type property
- GIVEN schema `InteractionSseEvent` has discriminator `propertyName: "event_type"` with no `mapping`
- AND variant `InteractionStatusUpdate` has `properties.event_type.const = "interaction.status_update"`
- WHEN `build.rs` generates the `InteractionSseEvent` enum
- THEN the variant is annotated `#[serde(rename = "interaction.status_update")]`

### Scenario: Fallback to derive_tag_from_variant when no const
- GIVEN a variant whose target schema has no `const` on the discriminator property
- WHEN `build.rs` resolves the tag
- THEN it falls back to `derive_tag_from_variant()` (existing behavior)

## Requirement: translate_stream_event Uses Typed InteractionSseEvent

`translate_stream_event` deserializes each SSE data line into `InteractionSseEvent` using serde's tag dispatch. Manual `event_type` string matching is removed.

### Scenario: Typed event dispatch
- GIVEN an SSE data line with `"event_type": "step.delta"`
- WHEN `serde_json::from_str::<InteractionSseEvent>(data)` is called
- THEN it deserializes as `InteractionSseEvent::StepDelta(StepDelta { ... })`
- AND the match arm receives typed data without manual string comparison

## Requirement: Isolate JSON Roundtrip Construction

When `serde_json::json!({...})` is needed to construct a value (because the target type's inner types are not publicly exported by `anyllm_translate`), the `json!` call must be isolated in a dedicated factory function with minimal scope. Callers receive a strongly-typed value and never interact with `serde_json::json!` directly.

**Rules:**
- Each factory function constructs exactly one type variant (e.g. `stream_event_message_start`, `openai_sse_role_chunk`)
- The factory body contains the `serde_json::json!({...})` + serialization/deserialization
- A section comment above the factory block explains **why** the roundtrip is necessary (inner types not public)
- Callers use the factory function name, not inline `json!` — resulting in readable, type-checked code

### Scenario: StreamEvent constructed via factory
- GIVEN a `StreamEvent::MessageStart` is needed
- WHEN reading the call site
- THEN the code reads `stream_event_message_start(id, model, 0, 0)` — no `serde_json::from_value(serde_json::json!({...}))` inline

### Scenario: OpenAI SSE chunk constructed via factory
- GIVEN an OpenAI `chat.completion.chunk` SSE line is needed
- WHEN reading the call site
- THEN the code reads `openai_sse_role_chunk(msg_id, model, index)` — the `serde_json::json!` call is hidden inside the factory

### Scenario: New variant added
- GIVEN a new event variant requires `serde_json::json!` construction
- WHEN adding support
- THEN a new factory function is added to the same isolated block, following the existing naming and comment conventions

## Requirement: Handling Policy for Unsupported-but-Valid Events

Events that are valid (correctly deserialized) but not fully supported fall into three categories with distinct behavior:

| Category | Example | Behavior | Rationale |
|----------|---------|----------|-----------|
| **Malformed data** | Invalid JSON, unknown `event_type` | `tracing::info!` with raw data prefix (first 200 chars), then drop | `info!` (not `warn!`) because protocol evolution may introduce new event types that are safe to ignore |
| **No client impact** | `interaction.status_update` | Skip silently; code comment explains why | `tracing::warn!` would produce noise for an expected, harmless event |
| **Not yet implemented** | Unhandled delta types (`image_delta`, `audio_delta`, etc.) | `tracing::warn!` with event type; stream continues | Makes the gap visible to operators; serves as a signal to prioritize implementation |

### Scenario: Malformed event logged then dropped
- GIVEN an SSE data line `{"event_type": "future.unknown_event", "payload": ...}`
- WHEN `serde_json::from_str::<InteractionSseEvent>` returns `Err`
- THEN `tracing::info!` is emitted with the first 200 chars of the raw data
- AND `None` is returned (event dropped, stream continues)

### Scenario: interaction.status_update skipped silently
- GIVEN an SSE event `{"event_type": "interaction.status_update", "status": "in_progress"}`
- WHEN `translate_stream_event` processes it
- THEN it returns `None` (event skipped)
- AND a code comment above the `InteractionStatusUpdate` match arm states "status updates have no client-visible effect; safe to skip"

### Scenario: Unhandled delta type logged
- GIVEN an SSE event `{"event_type": "step.delta", "delta": {"type": "image_delta", "image": "..."}}`
- WHEN `translate_stream_event` processes it and encounters an unhandled delta variant
- THEN `tracing::warn!(delta_type = "image_delta", "unhandled step.delta type, dropping")` is emitted
- AND `None` is returned (event dropped, stream continues)

## Requirement: Typed Construction Pipeline

Interactions request construction uses typed structs throughout, never raw `serde_json::Value`:

- `build_request_body()` takes typed scalars (`stream: bool`, `temperature: Option<f64>`, `ingress_max_tokens: Option<u32>`, `system_instruction: Option<String>`) and returns `CreateModelInteractionParams`
- `build_interactions_request_anthropic`/`build_interactions_request_openai` accept `&[Value]` messages + typed scalars, not a raw body `Value`
- `build_chunk_request()` helper constructs chunk requests with `model`, `input: Vec<Content>`, optional `system_instruction`/`previous_interaction_id`
- Split-path reads struct fields (`params.input`, `params.system_instruction`, `params.previous_interaction_id`)
- Serialization to HTTP body bytes happens at the call site via `serde_json::to_vec(&params)`

### Scenario: Request body never converted to/from JSON
- GIVEN parsed ingress values (`model`, `stream`, `temperature`, `messages`, `system`)
- WHEN `build_interactions_request_anthropic` constructs the request
- THEN all parameters are typed scalars or typed structs
- AND the return value is `CreateModelInteractionParams` (not `serde_json::Value`)

## Requirement: Parse at Ingress Boundary

Raw `serde_json::Value` must not thread through functions when typed equivalents exist. Ingress JSON is parsed into typed structs at the protocol boundary. All downstream functions receive typed parameters.

### Scenario: Control message cleaning on typed messages
- GIVEN `messages: Vec<Value>` from parsed ingress body
- WHEN `scan_control_messages` processes the messages
- THEN cleaned messages are passed directly to build functions (no JSON body clone + mutation)

## Requirement: Generated Struct Defaults

Selected generated structs derive `Default` for ergonomic construction with `..Default::default()`:

- `CreateModelInteractionParams` — ~20 optional fields default to `None`
- `Function` — `name`, `description`, `parameters` default to `None`/`Null`
- `GenerationConfig` — all `Option<T>` fields default to `None`
- `TextContent` — `annotations` defaults to `None`, `r#type` to `Value::Null` (skipped on serialize)
- `Interaction`, `InteractionStatusUpdate`, `ModelOutputStep`

`InteractionsInput` has a manual `impl Default` (returns `String("")` variant — only used as struct field default, never sent over the wire).

### Scenario: Ergonomic struct construction
- GIVEN `TextContent { text: "hello".into(), ..Default::default() }`
- WHEN serialized as part of `Content::TextContent`
- THEN output has `{"text":"hello","type":"text"}` — no `null` fields

## Requirement: Gemini Error Body Translation (Non-Streaming)

When the Interactions API returns a non-2xx status with a Gemini-shaped error body `{"error":{"message":"...","code":"..."}}`, the proxy translates it to the ingress protocol format before applying user-configured `error_translation` rules. The function `translate_interactions_error_to_protocol(body: &str, ingress: Protocol)` in `src/lib.rs` handles this.

All 4 non-streaming error paths in `InteractionsHandler` call it before `apply_error_translation`:
- `send_and_translate`
- `handle_split_send`
- `send_split_system_instruction` (both error sites)

### Scenario: Gemini error → Anthropic format
- GIVEN interactions upstream returns body `{"error":{"message":"Quota exceeded","code":"too_many_requests"}}`
- WHEN `translate_interactions_error_to_protocol` is called with `Protocol::Anthropic`
- THEN the body is translated to `{"type":"error","error":{"type":"too_many_requests","message":"Quota exceeded"}}`

### Scenario: Gemini error → OpenAI format
- GIVEN interactions upstream returns body `{"error":{"message":"Quota exceeded","code":"too_many_requests"}}`
- WHEN `translate_interactions_error_to_protocol` is called with `Protocol::OpenAi`
- THEN the body is translated to `{"error":{"message":"Quota exceeded","type":"too_many_requests","code":"too_many_requests"}}`

### Scenario: Missing code defaults to api_error
- GIVEN interactions upstream returns body `{"error":{"message":"Internal error"}}` without `code` field
- WHEN `translate_interactions_error_to_protocol` is called
- THEN `error.type` defaults to `"api_error"`

### Scenario: Non-Gemini body passes through
- GIVEN interactions upstream returns a body that is not valid JSON or lacks `error.message`
- WHEN `translate_interactions_error_to_protocol` is called
- THEN the body is returned unchanged

### Scenario: User rule overrides translated body
- GIVEN `translate_interactions_error_to_protocol` translates a Gemini error to Anthropic format
- AND a user-configured `[[error_translation]]` rule matches (e.g., `status = 429`)
- THEN `apply_error_translation` replaces the body with the rule's `egress`

## Requirement: Shared Non-UTF-8 Upstream Body Validation

`validate_upstream_body(body: Bytes, request_id: &str) -> Result<ValidatedBody, AppError>` in `src/lib.rs` detects non-UTF-8 upstream response bodies. On success it returns `ValidatedBody { text, dump }` with the decoded string and a `DumpBody` ready for `response_dump`. On failure it logs `tracing::warn!("non-utf8 upstream response body")` and returns `AppError::Internal("non-utf8 response from upstream")`.

Used by all three handlers (`openai.rs`, `anthropic.rs`, `interactions_handler.rs`) for non-streaming responses.

The interactions streaming path (`handle_stream_response`) also detects non-UTF-8 chunks — replacing the previous `String::from_utf8_lossy` (which silently produced garbage). On a binary chunk it sends an SSE `error` event with `{"type":"error","error":{"type":"upstream_error","message":"non-utf8 response from upstream"}}` and aborts the stream.

### Scenario: Binary upstream response detected (non-streaming)
- GIVEN upstream returns bytes `0xFF 0xFE 0x00` with `content-type: application/json`
- WHEN `validate_upstream_body` is called
- THEN `tracing::warn!` is emitted
- AND `AppError::Internal("non-utf8 response from upstream")` is returned

### Scenario: Valid UTF-8 passes through
- GIVEN upstream returns valid UTF-8 JSON bytes
- WHEN `validate_upstream_body` is called
- THEN `Ok(ValidatedBody { text, dump })` is returned

### Scenario: Binary chunk in interactions stream
- GIVEN an interactions SSE stream with a non-UTF-8 chunk
- WHEN the chunk is received by `handle_stream_response`
- THEN an SSE `error` event is sent to the client
- AND the stream is aborted with `finish_with_error(502, ...)`
