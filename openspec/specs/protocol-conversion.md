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
- `anthropic-ratelimit-requests-limit`
- `anthropic-ratelimit-requests-remaining`
- `anthropic-ratelimit-requests-reset`
- `anthropic-ratelimit-tokens-limit`
- `anthropic-ratelimit-tokens-remaining`
- `anthropic-ratelimit-tokens-reset`

All other upstream response headers are filtered out. The OpenAI response relay path applies its own header forwarding via `relay_response_headers`.

## Requirement: Anthropic → Interactions Translation

`InteractionsHandler` converts Anthropic ingress to `CreateModelInteractionParams`:

- Ingress body parsed at boundary — `model`, `stream`, `temperature`, `max_tokens` extracted as typed scalars
- `messages[]` → interactions `Content[]` via typed extractors
- `system` → `system_instruction` (extracted by `extract_anthropic_system`)
- `max_tokens` → `generation_config.max_output_tokens`
- `tools` and `tool_choice` extracted from ingress body via `extract_anthropic_tools`, converted to Interactions API format (`Vec<Tool>` and `ToolChoice`), set on `CreateModelInteractionParams.tools` and `generation_config.tool_choice`
- `previous_interaction_id` set from session state (if exists)
- All parameters passed as typed scalars to `build_interactions_request_anthropic`, which returns `CreateModelInteractionParams` directly
- Split-path accesses struct fields (`params.input`, `params.system_instruction`, `params.previous_interaction_id`) — no `.get()` on `serde_json::Value`

Only messages not yet delivered to the session are included (delta computation). Control messages are stripped before construction.

### Scenario: First request in session
- GIVEN no prior session state
- WHEN Anthropic request with 3 messages arrives
- THEN all 3 messages are translated, no `previous_interaction_id` sent

### Scenario: Subsequent request — delta + chain
- GIVEN session has `{interaction_id: "abc123", delivered_count: 3}`
- WHEN Anthropic request with 5 messages arrives (same session)
- THEN only messages [3..5] are sent, `previous_interaction_id: "abc123"` is set

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
- System message (role=system) → `system_instruction`
- `max_tokens` → `generation_config.max_output_tokens`
- `tools` and `tool_choice` extracted from ingress body via `extract_openai_tools`, converted to Interactions API format (`Vec<Tool>` and `ToolChoice`), set on `CreateModelInteractionParams.tools` and `generation_config.tool_choice`
- All parameters passed as typed scalars to `build_interactions_request_openai`, which returns `CreateModelInteractionParams` directly

### Scenario: OpenAI tools forwarded to interactions API
- GIVEN OpenAI ingress body with `"tools": [{"type": "function", "function": {"name": "search"}}]` and `"tool_choice": "auto"`
- WHEN the interactions request is built
- THEN `CreateModelInteractionParams.tools` contains the tool definitions
- AND `generation_config.tool_choice` reflects the tool choice

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

## Requirement: Document JSON Roundtrip Usage

When `StreamEvent` variants are constructed via `serde_json::from_value(serde_json::json!({...}))` because their inner types are not publicly exported by `anyllm_translate`, the code must include a comment on the same line block explaining **why** a direct constructor cannot be used.

### Scenario: All JSON roundtrip sites are annotated
- GIVEN any `serde_json::from_value(serde_json::json!({...}))` call that constructs a `StreamEvent`
- WHEN reading the code
- THEN a nearby comment explains that the inner type is not public in `anyllm_translate`

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
