# Delta: Protocol Conversion (Interactions)

**Change ID:** `fix-interactions-protocol-correctness`
**Affects:** `src/interactions_handler.rs`, `src/interactions.rs`, `src/session.rs`

---

## ADDED

### Requirement: Lifecycle Operations Preserve Endpoint Query Parameters

`build_interaction_url` must preserve query parameters from the configured `endpoint_interactions` URL when constructing lifecycle operation URLs (cancel, delete, get). The endpoint URL is parsed and the query string is reattached to the operation path.

#### Scenario: Query parameter preserved for lifecycle operations
- GIVEN `endpoint_interactions = "https://host/v1beta/interactions?key=ABC"`
- WHEN `build_interaction_url` constructs a cancel/delete/get URL for interaction `int-1`
- THEN the URL is `https://host/v1beta/interactions/int-1:cancel?key=ABC`
- AND the `x-goog-api-key` header is set as normal

#### Scenario: No query parameter — no change
- GIVEN `endpoint_interactions = "https://host/v1beta/interactions"` (no query string)
- WHEN `build_interaction_url` constructs a lifecycle URL
- THEN the URL is `https://host/v1beta/interactions/int-1:cancel` (unchanged behavior)

### Requirement: max_tokens is a Cap, Not an Override

`build_request_body` must use `min(client_max_tokens, route.max_tokens)` semantics: if both the client and the route specify a token limit, the lower (more restrictive) value wins. `route.max_tokens.or(ingress_max_tokens)` is replaced with a `min`-based approach.

#### Scenario: Client limit lower than route — client wins
- GIVEN client sends `max_tokens = 100` and route has `max_tokens = 1000`
- WHEN `build_request_body` constructs generation_config
- THEN `max_output_tokens` is `100` (client's more restrictive limit)

#### Scenario: Route limit lower than client — route wins
- GIVEN client sends `max_tokens = 1000` and route has `max_tokens = 100`
- WHEN `build_request_body` constructs generation_config
- THEN `max_output_tokens` is `100` (route caps the client)

#### Scenario: No route limit — client value used
- GIVEN client sends `max_tokens = 500` and route has no `max_tokens`
- WHEN `build_request_body` constructs generation_config
- THEN `max_output_tokens` is `500`

### Requirement: OpenAI max_completion_tokens Respected in Interactions Path

`handle_from_openai` must read both `max_completion_tokens` and `max_tokens` from the ingress body. When only `max_completion_tokens` is present, it is used as the token limit. When both are present, `max_completion_tokens` takes precedence (standard OpenAI semantics).

#### Scenario: Only max_completion_tokens sent
- GIVEN client sends `{"max_completion_tokens": 200}` without `max_tokens`
- WHEN `handle_from_openai` processes the request
- THEN `ingress_max_tokens` is `Some(200)`
- AND `generation_config.max_output_tokens` is `200`

#### Scenario: Both max_tokens and max_completion_tokens
- GIVEN client sends `{"max_completion_tokens": 200, "max_tokens": 100}`
- WHEN `handle_from_openai` processes the request
- THEN `max_completion_tokens` takes precedence → `Some(200)`

### Requirement: u64 Token Counts Use Saturating Conversion

`Interaction.usage.total_input_tokens` and `total_output_tokens` (both `u64`) must use `u32::try_from()` with saturating fallback and `tracing::warn!` instead of silent `as u32` wrapping in release builds.

#### Scenario: Token count above u32::MAX
- GIVEN upstream reports `total_input_tokens = 5000000000` (> u32::MAX)
- WHEN translated to ingress format (Anthropic `Usage` or OpenAI `CompletionTokensDetails`)
- THEN the value is clamped to `u32::MAX`
- AND `tracing::warn!` logs the clamp

#### Scenario: Token count within u32 range
- GIVEN upstream reports `total_input_tokens = 15000`
- WHEN translated to ingress format
- THEN the value is `15000` (unchanged)

---

## MODIFIED

### Requirement: Anthropic → Interactions Translation

... (unchanged preamble) ...

`previous_interaction_id` must be cleared (set to `None`) when `compute_delta` returns `start_index == 0`, indicating a context reset. Previously `previous_interaction_id` from session state was retained even when the client started a fresh conversation with fewer messages than the session's `message_count`.

#### Scenario: Context reset — previous_interaction_id cleared
- GIVEN session has `{interaction_id: "int-1", message_count: 5}`
- WHEN client sends 2 messages (fresh conversation, fewer than 5)
- THEN `compute_delta(5, 2)` returns `start_index = 0`
- AND `previous_interaction_id` is **cleared to `None`** (was `Some("int-1")`)
- AND `system_instruction`, `tools`, and `generation_config` are included (new interaction)

#### Scenario: Normal continuation — previous_interaction_id kept
- GIVEN session has `{interaction_id: "int-1", message_count: 3}`
- WHEN client sends 5 messages (same conversation, more than 3)
- THEN `compute_delta(3, 5)` returns `start_index = 3`
- AND `previous_interaction_id` is `Some("int-1")` (unchanged)

### Requirement: Interactions Streaming Events

Streaming from interactions endpoint returns SSE with discriminated event types. The `handle_stream_response` task must update the session `interaction_id` eagerly from `InteractionCreatedEvent` before the stream completes, so that client-disconnect recovery has a valid interaction ID to probe.

#### Scenario: interaction_id set eagerly from InteractionCreatedEvent
- GIVEN a streaming interactions request
- AND session is initially `{pending: true, interaction_id: ""}`
- WHEN upstream emits `InteractionCreatedEvent` with `interaction.id = "int-2"`
- THEN `session_store.update` is called immediately with `interaction_id = "int-2"` (pending remains true)
- AND if the client disconnects afterward, startup recovery probes `int-2` (not empty string)

### Requirement: Interactions → Anthropic Translation

`InteractionCompletedEvent` must NOT emit `ContentBlockStop` when the preceding `StepStop` event already stopped the same block index. The last active block index is tracked from `step.start` events, and the duplicate emission at line 2369 is removed.

#### Scenario: InteractionCompletedEvent after StepStop — no duplicate stop
- GIVEN upstream emits `StepStop(index=0)` then `InteractionCompletedEvent`
- WHEN `translate_stream_event` processes `InteractionCompletedEvent`
- THEN `message_delta` + `message_stop` are emitted
- AND NO `content_block_stop` for index 0 is emitted (already stopped by StepStop)

### Requirement: Proxy-Limit Split-Send Chunk Forwarding

... (unchanged preamble) ...

**Session updates:** `message_count` must track ingress message count (after control message stripping), not Content item count. Content item count can differ when ingress messages include tool results or system messages that map to multiple or zero Content items.

#### Scenario: message_count tracks ingress messages, not Content items
- GIVEN 5 ingress messages (after control stripping): [user, assistant(tool_use), user(tool_result), assistant, user]
- AND `extract_*_content` produces 4 Content items (tool_result mapped differently)
- WHEN session is updated after chunk delivery
- THEN `message_count` increments by 5 (ingress message count)
- NOT by 4 (Content item count)

**Session finalization:** `handle_split_send` must translate the final response and build the client response BEFORE marking the session `pending = false`. Previously the session was finalized first, making translation failures unrecoverable.

#### Scenario: Translation failure after session finalized — unrecoverable retry
- GIVEN all chunks succeed and session is updated to `pending = false`
- WHEN `build_response_from_interaction` then fails
- THEN the error is returned to the client
- AND on retry, `compute_delta` sees all messages delivered → sends empty input → unrecoverable
- THEREFORE: session must be finalized ONLY after successful translation

### Requirement: Interactions → OpenAI Translation

... (unchanged preamble) ...

`extract_openai_system` must scan ALL message positions for `role: "system"`, not just `messages.first()`. System messages can appear at any position in OpenAI message arrays.

#### Scenario: System message in non-first position
- GIVEN OpenAI messages `[{role: "user", ...}, {role: "system", content: "Be concise"}, ...]`
- WHEN `extract_openai_system` is called
- THEN the system instruction `"Be concise"` is extracted
- AND upstream receives it as `system_instruction`

### Requirement: compute_delta Empty Delta for Exact Retries

When `compute_delta` returns an empty delta (`start_index == ingress_count`) and `previous_interaction_id` is `Some`, the handler must fetch the existing interaction via `GET /v1beta/interactions/{id}` and return its result instead of sending an empty `ContentList` upstream.

#### Scenario: Exact retry recovers existing interaction
- GIVEN session has `{interaction_id: "int-1", message_count: 5}`
- WHEN client retries with the same 5 messages
- THEN `compute_delta(5, 5)` returns `(start=5, end=5)` → empty delta
- AND `previous_interaction_id` is `Some("int-1")`
- THEN handler calls `GET /v1beta/interactions/int-1` to retrieve the existing interaction
- AND returns the translated response to the client
- INSTEAD OF sending empty input upstream (which would return an error or empty response)

### Requirement: System-Instruction Splitting Uses Full Body Measurement

The proxy_limit check in system-instruction splitting must measure the full serialized `CreateModelInteractionParams` body (including tools, generation_config, model, and all non-content fields), not just the text length of the system instruction.

#### Scenario: Envelope overhead causes oversized request
- GIVEN `proxy_limit = 100KB`, tools + generation_config serialized = 60KB, system_instruction = 50KB
- WHEN checking if system instruction needs splitting
- THEN the check measures `serialize(envelope + system_instruction + empty_input)` ≈ 110KB > limit
- AND `split_text_for_limit` splits the text so each chunk + envelope fits within 100KB
- INSTEAD OF: checking text-only (50KB < 100KB) and sending 110KB oversized

### Requirement: send_split_system_instruction Propagates Errors

`send_split_system_instruction` must propagate deserialization failures from upstream responses instead of silently dropping them via `if let Ok`. The error must interrupt the chunk chain so that stale `current_prev` is not used for subsequent chunks.

#### Scenario: Malformed upstream response propagated
- GIVEN upstream returns HTTP 200 with malformed JSON for a system-instruction chunk
- WHEN `serde_json::from_str::<Interaction>` fails
- THEN the error is returned (NOT silently dropped via `if let Ok`)
- AND no subsequent chunk is sent with stale `previous_interaction_id`
- AND the client receives an appropriate error

---

## REMOVED

(None)
