# Delta: Protocol Conversion

**Change ID:** `redesign-session-state-model`
**Affects:** `openspec/specs/protocol-conversion.md`, `src/interactions.rs`, `src/interactions_handler.rs`, `src/sse.rs`

---

## MODIFIED

### Requirement: Anthropic -> Interactions Translation

Anthropic ingress still converts to `CreateModelInteractionParams`, but delta selection changes from `compute_delta(message_count, incoming)` to hash frontier selection over filtered harness messages.

Rules:
- Control messages are stripped before hashing and conversion.
- Harness messages are Anthropic `user` messages only.
- `previous_interaction_id` is the terminal interaction returned by longest valid prefix selection.
- `system_instruction`, `tools`, `tool_choice`, and `generation_config` are included only when `previous_interaction_id` is `None`.
- If all incoming harness messages are known, no upstream create call is made; the terminal interaction is replayed.

#### Scenario: Subsequent request — hash delta + chain
- GIVEN known Anthropic harness hashes `[0xA, 0xB]` ending at `int-2`
- WHEN request contains user hashes `[0xA, 0xB, 0xC]`
- THEN only message `0xC` is translated
- AND `previous_interaction_id = "int-2"`
- AND first-interaction fields are absent

#### Scenario: History rewrite starts new interaction
- GIVEN known hashes `[0xA, 0xB]`
- WHEN incoming user hashes are `[0xA, 0xX]`
- THEN longest valid prefix is `[0xA]`
- AND only messages from `0xX` onward are sent with `previous_interaction_id` for the `[0xA]` terminal chain

#### Scenario: No known prefix includes first-interaction fields
- GIVEN no valid prefix for incoming user hashes
- WHEN Anthropic request is built
- THEN all user messages are sent
- AND `system_instruction`, `tools`, and `generation_config` are included

### Requirement: OpenAI -> Interactions Translation

OpenAI ingress still converts to `CreateModelInteractionParams`, but delta selection changes from raw message count to hash frontier selection over filtered harness messages.

Rules:
- Control messages are stripped before hashing and conversion.
- Harness messages are roles `system`, `developer`, `user`, and `tool`.
- `assistant` messages are ignored for frontier and are not sent upstream.
- `previous_interaction_id` comes from longest valid prefix selection.
- `system_instruction`, `tools`, `tool_choice`, and `generation_config` are included only when `previous_interaction_id` is `None`.
- If all incoming harness messages are known, no upstream create call is made; the terminal interaction is replayed.

#### Scenario: Assistant history ignored
- GIVEN OpenAI request messages are `[system, user, assistant, tool]`
- WHEN harness messages are filtered
- THEN hashes are computed only for `[system, user, tool]`

#### Scenario: Subsequent request omits first-interaction fields
- GIVEN known harness prefix ends at `int-2`
- WHEN OpenAI request adds one new `user` message
- THEN only that user message is sent
- AND `previous_interaction_id = "int-2"`
- AND tools/system/generation config are absent

### Requirement: Proxy-Limit Split-Send Chunk Forwarding

Existing full-body split-send invariants MUST be preserved while state tracking changes to `InFlightStore`.

Split packing MUST:
- measure full serialized `CreateModelInteractionParams` body;
- split `system_instruction` before content when needed;
- include `tools`, `generation_config`, and `system_instruction` on first chunk only;
- include `previous_interaction_id` on later chunks;
- account for serialized `previous_interaction_id` overhead during size estimation;
- reject unsplittable requests before sending any piece.

State updates MUST use `InFlightBatch` piece statuses, not per-chunk `message_count` updates. The terminal interaction node is inserted only after all pieces ACK.

#### Scenario: Split terminal node owns harness hashes
- GIVEN one harness message splits into two upstream chunks
- WHEN both chunks ACK
- THEN only final interaction id is indexed for the original harness message hash

#### Scenario: Failed second chunk leaves no terminal node
- GIVEN first chunk ACKs `int-A`
- WHEN second chunk fails
- THEN `int-A` is cancelled best-effort
- AND no `InteractionNode` is inserted for the original harness message

#### Scenario: System instruction split preserves first fields
- GIVEN system instruction exceeds `proxy_limit`
- WHEN split-send starts
- THEN first system chunk carries tools and generation config
- AND later chunks chain via `previous_interaction_id`

### Requirement: Split-Send Streaming with Buffered SSE

When original ingress has `stream: true` and split-send is required, the handler MUST buffer all upstream piece SSE responses until the final interaction id is known, substitute intermediate ids with the final id, then emit one coherent client-visible SSE stream.

For Anthropic clients, buffered interactions SSE is translated to Anthropic `StreamEvent` SSE after substitution.
For OpenAI clients, the substituted Anthropic-style stream is passed through `ReverseStreamingTranslator` to produce OpenAI chat-completion chunks.

The initial buffer is memory-backed with 100 MB limit. On overflow, ACKed piece interactions are cancelled best-effort and the batch is marked failed.

#### Scenario: Two-piece streaming response uses final id
- GIVEN P0 creates `int-A` and P1 creates `int-B`
- WHEN client receives streamed response
- THEN all client-visible message/interaction identifiers reference `int-B`
- AND no event exposes `int-A`

#### Scenario: Buffer overflow fails safely
- GIVEN buffered split-send SSE exceeds 100 MB
- WHEN overflow is detected
- THEN ACKed pieces are cancelled best-effort
- AND the client receives an error response/event
