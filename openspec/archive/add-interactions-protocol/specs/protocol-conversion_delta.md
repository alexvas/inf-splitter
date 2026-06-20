# Delta: Protocol Conversion

**Change ID:** `add-interactions-protocol`
**Affects:** `src/interactions.rs` (new), `src/openai.rs`, `src/anthropic.rs`

---

## ADDED

### Requirement: Anthropic → Interactions Translation

`InteractionsHandler` converts Anthropic `MessageCreateRequest` to `CreateModelInteractionParams`:

- Anthropic `messages[]` → `InteractionsInput` as `Content[]` (text blocks → `TextContent`, image blocks → `ImageContent`)
- Anthropic `system` (top-level or system message) → `system_instruction` field
- Anthropic `max_tokens` → `generation_config.max_output_tokens`
- Anthropic `temperature`, `top_p`, `top_k` → `generation_config` equivalents
- `previous_interaction_id` set from session state (if exists)

**Delta handling:** Only messages not yet delivered to the session are included in the translated `input`.

#### Scenario: First request in session
- GIVEN no prior session state
- WHEN Anthropic request with 3 messages arrives
- THEN all 3 messages are translated to `Content[]`, no `previous_interaction_id` sent

#### Scenario: Subsequent request — delta + chain
- GIVEN session has `{interaction_id: "abc123", delivered_count: 3}`
- WHEN Anthropic request with 5 messages arrives (same session)
- THEN only messages [3..5] are sent as `Content[]`, `previous_interaction_id: "abc123"` is set

### Requirement: Interactions → Anthropic Translation

`Interaction` response is translated to Anthropic `MessageResponse`:
- `Interaction.steps[]` filtered for `ModelOutputStep` → Anthropic `content[]` text blocks
- `Interaction.usage` → response metadata
- Stream: `ContentDelta` events → Anthropic `StreamEvent` SSE
- Stream: `InteractionCompletedEvent` → final `MessageResponse` with `stop_reason: "end_turn"`

#### Scenario: Text response from interactions
- GIVEN `Interaction` with `ModelOutputStep` containing text
- WHEN translated to Anthropic format
- THEN response has `{"type": "message", "content": [{"type": "text", "text": "..."}], "stop_reason": "end_turn", ...}`

### Requirement: OpenAI → Interactions Translation

OpenAI `ChatCompletionRequest` → `CreateModelInteractionParams`:
- OpenAI `messages[]` → `InteractionsInput` as `Content[]`
- OpenAI `max_tokens` → `generation_config.max_output_tokens`

### Requirement: Interactions → OpenAI Translation

`Interaction` → OpenAI `ChatCompletionResponse`:
- `Interaction.steps[]` → `choices[].message.content`
- Stream: `ContentDelta` → OpenAI streaming chunks, `InteractionCompletedEvent` → `[DONE]`

### Requirement: Interactions Streaming Events

Streaming from interactions endpoint returns SSE with discriminated event types:

| Event type | Property | Meaning |
|-----------|----------|---------|
| `InteractionCreatedEvent` | `interaction: Interaction` | Interaction created, contains full initial state |
| `ContentDelta` | content chunks | Incremental text/image/audio output |
| `InteractionCompletedEvent` | `interaction: Interaction` | Final interaction (outputs empty — use ContentDelta) |
| `ErrorEvent` | `error: Error` | Stream-level error |

Events have `event_id` for resume support and `metadata` with `total_usage`.

#### Scenario: Streaming Anthropic ingress → Interactions upstream
- GIVEN `POST /v1/messages` with `"stream": true`
- WHEN routing to interactions endpoint with `stream: true`
- THEN `ContentDelta` events translated to Anthropic SSE format, `InteractionCompletedEvent` produces final message

#### Scenario: Streaming OpenAI ingress → Interactions upstream
- GIVEN `POST /v1/chat/completions` with `"stream": true`
- WHEN routing to interactions endpoint with `stream: true`
- THEN `ContentDelta` events translated to OpenAI SSE chunks, `[DONE]` on completion

### Requirement: Interactions Request/Response Types

Rust types for the interactions protocol are generated at build time from `schemas/interactions.openapi.json` by `build.rs`. The generated code is included in `src/interactions_types.rs` via `include!`. This covers:

- `InteractionsRequest` — request body for `POST /v1beta/interactions`
- `InteractionsResponse` — non-streaming response
- Stream event types — SSE chunk parsing
- Content/Part types — multimodal content
- `GenerationConfig` — temperature, top_p, top_k, max_output_tokens

#### Scenario: Schema is committed
- GIVEN `schemas/interactions.openapi.json` exists in the repo
- WHEN `cargo build` runs
- THEN build.rs generates types without network access

---

## MODIFIED

### Requirement: OpenAI→Anthropic Translation

(Unchanged — existing paths via `endpoint_openai`/`endpoint_anthropic` remain identical.)

### Requirement: Anthropic→OpenAI Translation

(Unchanged.)

---

## REMOVED

(None)
