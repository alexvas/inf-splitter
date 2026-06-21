# Delta: Protocol Conversion (Interactions Streaming Events)

**Change ID:** `fix-interactions-stream-event-translation`
**Affects:** `build.rs` — oneOf discriminator tag inference; `src/interactions_handler.rs` — `translate_stream_event` function; `src/interactions_types.rs` — regenerated

---

## ADDED

### Requirement: build.rs Infers OneOf Discriminator Tags from const

When a oneOf discriminator schema has no explicit `mapping`, `build.rs` must inspect each variant's target schema for a `const` value on the discriminator property. If found, this value becomes the `#[serde(rename)]` tag for the enum variant.

#### Scenario: Tag inferred from const on event_type property
- GIVEN schema `InteractionSseEvent` has discriminator `propertyName: "event_type"` with no `mapping`
- AND variant `InteractionStatusUpdate` has `properties.event_type.const = "interaction.status_update"`
- WHEN `build.rs` generates the `InteractionSseEvent` enum
- THEN the variant is annotated `#[serde(rename = "interaction.status_update")]`

#### Scenario: Fallback to derive_tag_from_variant when no const
- GIVEN a variant whose target schema has no `const` on the discriminator property
- WHEN `build.rs` resolves the tag
- THEN it falls back to `derive_tag_from_variant()` (existing behavior)

### Requirement: translate_stream_event Uses Typed InteractionSseEvent

`translate_stream_event` deserializes each SSE data line into `InteractionSseEvent` using serde's tag dispatch. Manual `event_type` string matching is removed.

#### Scenario: Typed event dispatch
- GIVEN an SSE data line with `"event_type": "step.delta"`
- WHEN `serde_json::from_str::<InteractionSseEvent>(data)` is called
- THEN it deserializes as `InteractionSseEvent::StepDelta(StepDelta { ... })`
- AND the match arm receives typed data without manual string comparison

### Requirement: Document JSON Roundtrip Usage

When `StreamEvent` variants are constructed via `serde_json::from_value(serde_json::json!({...}))` because their inner types are not publicly exported by `anyllm_translate`, the code must include a comment explaining **why** a direct constructor cannot be used.

### Requirement: Handling Policy for Unsupported-but-Valid Events

Events that are valid (correctly deserialized) but not fully supported fall into three categories with distinct behavior:

| Category | Example | Behavior | Rationale |
|----------|---------|----------|-----------|
| **Malformed data** | Invalid JSON, unknown `event_type` | `tracing::info!` with raw data prefix (first 200 chars), then drop | `info!` not `warn!` — protocol evolution may introduce new types that are safe to ignore |
| **No client impact** | `interaction.status_update` | Skip silently; code comment explains why | `tracing::warn!` would produce noise for an expected, harmless event |
| **Not yet implemented** | Unhandled delta types | `tracing::warn!` with event type; stream continues | Makes the gap visible to operators |

---

## MODIFIED

### Requirement: Interactions Streaming Events

Streaming from interactions endpoint returns SSE with discriminated event types:

| Event type | Meaning |
|-----------|---------|
| `interaction.created` | Interaction created, contains full initial state |
| `interaction.status_update` | Status change (skipped in translation) |
| `step.start` | A new step begins (thought, model_output, tool_call, etc.) |
| `step.delta` | Incremental output for the current step (text, thought_signature, etc.) |
| `step.stop` | Current step completes, includes per-step usage |
| `error` | Stream-level error |
| `interaction.completed` | Final interaction with total usage |

> **Note:** The old `content.delta` / `ContentDelta` events are no longer emitted by the current Gemini Interactions API. The protocol now uses `step.*` events.

#### Scenario: Streaming Anthropic ingress → Interactions upstream
- GIVEN `POST /v1/messages` with `"stream": true`
- WHEN routing to interactions endpoint with `stream: true`
- THEN SSE events translated to Anthropic SSE format:
  - `interaction.created` → `message_start` + `content_block_start` (initial text block)
  - `step.start` → `content_block_start`
  - `step.delta` (text) → `content_block_delta { type: "text_delta" }`
  - `step.delta` (thought_signature) → `content_block_delta { type: "signature_delta" }`
  - `step.stop` → `content_block_stop`
  - `error` → `event: error`
  - `interaction.completed` → `message_delta { stop_reason }` + `message_stop`

#### Scenario: Full stream lifecycle with thinking
- GIVEN an interactions response with `interaction.created` → `step.start (thought)` → `step.delta (signature)` → `step.stop` → `step.start (model_output)` → `step.delta (text)` → `step.stop` → `interaction.completed`
- WHEN the proxy translates the stream to Anthropic format
- THEN the client receives the full sequence of message_start, content_block_start, content_block_delta, content_block_stop, message_delta, message_stop events

#### Scenario: Error event translation
- GIVEN an interactions SSE event `{"event_type": "error", "error": {"code": "not_found", "message": "Result not found."}}`
- WHEN the proxy translates it to Anthropic streaming format
- THEN the client receives `event: error` with `{"type": "error", "error": {"type": "not_found", "message": "Result not found."}}`

---

## REMOVED

(None)
