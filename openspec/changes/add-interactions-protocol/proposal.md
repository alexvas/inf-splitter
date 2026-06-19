# Proposal: Add Gemini Interactions API Support

**Change ID:** `add-interactions-protocol`
**Created:** 2026-06-18
**Status:** Draft

---

## Problem Statement

The proxy currently supports two upstream protocols: OpenAI-compatible and Anthropic-compatible. Google's Gemini Interactions API (`POST /v1beta/interactions`) is a **stateful** protocol — the server tracks conversation history internally. This enables efficient multi-turn conversations where only new messages need to be sent.

The challenge is bridging **stateless** client protocols (Anthropic, OpenAI) to this **stateful** upstream protocol. The proxy must:
1. Track successfully delivered messages per session
2. Compute the delta between already-delivered and incoming messages
3. Send only the delta to the interactions endpoint
4. Translate between all three protocol formats

## Proposed Solution

### Build-time schema code generation

The OpenAPI schema from `https://ai.google.dev/static/api/interactions.openapi.json` is committed to the repo as `schemas/interactions.openapi.json` (152 schemas, 11 paths). During compilation, `build.rs` reads this schema and generates Rust serde types into `OUT_DIR`:

- `CreateModelInteractionParams` — request body (model, input, generation_config, system_instruction, previous_interaction_id, stream)
- `Interaction` — response body (id, status, steps, input, usage, model)
- `InteractionsInput` — `oneOf[string, Step[], Content[], Content]` input union
- `Step` — discriminated union: `UserInputStep`, `ModelOutputStep`, `ThoughtStep`, etc. (type discriminator)
- `Content` — discriminated union: `TextContent`, `ImageContent`, `AudioContent`, etc. (type discriminator)
- `GenerationConfig` — temperature, top_p, max_output_tokens, thinking_level, etc.
- Stream events: `InteractionCreatedEvent`, `ContentDelta`*, `InteractionCompletedEvent`, `ErrorEvent`
- `Usage` — token usage stats

### Config extension

Add `endpoint_interactions` field to provider sections:

```toml
[gemini]
endpoint_interactions = "https://generativelanguage.googleapis.com/v1beta/interactions?model"
api_key = "${GEMINI_API_KEY}"
models = "gemini-3.1-flash-lite"
```

The endpoint URL is used as-is for POST requests. The `?model` query parameter is included in the config URL (the model name is sent in the request body, not the URL).

### Session state tracking

The Interactions API is **stateful via chaining**: each interaction returns an `id`, and the next interaction references it via `previous_interaction_id`. The proxy tracks:

1. `interaction_id` from the last successful response
2. `message_count` — number of client messages successfully delivered (accounting for control messages and splits)

`SessionStore` maps `session_id` → `SessionState { interaction_id, message_count, last_access_utc, expires_at_utc, pending }`.

- `pending` — `true` if the interaction may not exist upstream yet (set on shutdown, verified on startup via GET)

**Session ID:** determined in priority order:

1. **HTTP header `x-request-id`** (primary) — de-facto standard; many Anthropic/OpenAI clients set this as a request header even though the specs define it as response-only. This is the most reliable source.
2. **Body field `request_id`** (fallback) — our proxy (inf-splitter) injects this for diagnostics in `{unix_ts}-{counter}` format
3. **Random UUID** (last resort) — if neither source is available, a random session ID is generated (no multi-turn delta optimisation, but fully functional)

No parsing or extraction logic — the value is used as-is.

### Session state tracking (details)

**How `message_count` works with various features:**

| Feature | Effect on `message_count` |
|---------|--------------------------|
| Normal request | `message_count += len(messages)` |
| Control messages | Stripped BEFORE counting — control messages are excluded from `message_count` |
| `proxy_limit` splitting | `message_count += total_messages_across_all_chunks` (the sum of all messages sent in all split interactions) |

The stored `interaction_id` always points to the LAST successful interaction (after all splits, if any), ensuring correct `previous_interaction_id` chaining.

### Delta computation

On each request:
1. Parse incoming Anthropic/OpenAI messages
2. Use `request_id` as `session_id`
3. Look up session state → get `{interaction_id, delivered_count}`
4. Take `messages[delivered_count..]` — only the new messages
5. Translate new messages to interactions `InteractionsInput` format (array of `Step` or `Content`)
6. Set `previous_interaction_id` from stored state
7. Send to interactions endpoint
8. On success: update session with new `interaction_id` and `delivered_count + new count`

### Protocol translation

**Anthropic → Interactions:**
- `MessageCreateRequest.messages[]` → `InteractionsInput` as `Content[]` or `Step[]`
- `MessageCreateRequest.system` → `system_instruction` field
- `MessageCreateRequest.max_tokens` → `generation_config.max_output_tokens`

**Interactions → Anthropic:**
- `Interaction.steps[]` (ModelOutputStep) → `MessageResponse.content[]` text blocks
- `Interaction.usage` → mapped to response metadata where applicable
- Stream: `ContentDelta` events → Anthropic `StreamEvent` SSE

**OpenAI → Interactions:**
- `ChatCompletionRequest.messages[]` → `InteractionsInput` as `Content[]` or `Step[]`
- `ChatCompletionRequest.max_tokens` → `generation_config.max_output_tokens`

**Interactions → OpenAI:**
- `Interaction.steps[]` → `ChatCompletionResponse.choices[].message.content`
- Stream: `ContentDelta` events → OpenAI streaming chunks

### Routing matrix (new)

| Ingress | Endpoint set | Handler | Action |
|---------|-------------|---------|--------|
| OpenAI | `endpoint_interactions` | `InteractionsHandler` | OpenAI→Interactions translate |
| Anthropic | `endpoint_interactions` | `InteractionsHandler` | Anthropic→Interactions translate |
| OpenAI | `endpoint_openai` | `OpenAiHandler` | Passthrough (unchanged) |
| Anthropic | `endpoint_anthropic` | `AnthropicHandler` | Passthrough (unchanged) |
| OpenAI | `endpoint_anthropic` | `AnthropicHandler` | OpenAI→Anthropic (unchanged) |
| Anthropic | `endpoint_openai` | `OpenAiHandler` | Anthropic→OpenAI (unchanged) |

When both `endpoint_interactions` and another endpoint are set, the matching ingress protocol uses its direct endpoint; the other ingress protocol converts to interactions.

### Auth

Interactions uses a minimal header set — only:
- `x-goog-api-key: {api_key}`
- `Content-Type: application/json`
- `Api-Revision: 2026-05-20`

No client request headers are forwarded to the interactions endpoint. The existing `forward_request_headers()` (which sets `x-api-key` and `Authorization`) is NOT used for interactions paths.

### Per-endpoint proxy

Provider sections can specify an explicit proxy for outgoing requests:

```toml
proxy = "http://127.0.0.1:8081"
# or
proxy = "socks5://172.17.0.1:3823"
```

If set, the reqwest client for that section is configured with `Proxy::all(url)`. If absent, reqwest uses environment variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`) — the default behaviour.

This applies to ALL handlers (OpenAi, Anthropic, Interactions) since it's a provider-level setting. The proxy is applied when constructing each handler's reqwest `Client` in `build_app()`.

### Health check

The interactions endpoint doesn't support HEAD. Health check uses a lightweight approach: if `endpoint_interactions` is configured, health probes `GET /v1beta/interactions` (expecting 400/405 rather than connection failure).

### Egress message splitting (proxy_limit)

When the translated `Content[]` or `Step[]` exceeds a configured byte-size limit, the proxy splits it into multiple interactions. Each sub-interaction is sent separately, chained via `previous_interaction_id`.

Config (per provider section):
```toml
proxy_limit = "130k"
```

**Algorithm:**
1. Translate messages to interactions format → `Content[]` array
2. Serialize to JSON, measure byte size
3. If size ≤ `proxy_limit` → send as single interaction (normal path)
4. If size > `proxy_limit` → split `Content[]` into chunks using **sequential greedy packing** (iterate in order, add to current chunk if it fits, otherwise start new chunk). Message order is preserved — no reordering.
   - First chunk: set `previous_interaction_id` from session state (if any previously delivered messages exist)
   - Subsequent chunks: chain via `previous_interaction_id` = previous chunk's `interaction.id`
   - Last chunk's `interaction.id` becomes the session's stored ID
5. If a single `Content` element alone exceeds `proxy_limit` → return error 415 with message `"Unable to split ingress message into chunks under proxy limit."`
6. **System instruction splitting:** if the outgoing request (even with empty `Content[]`) exceeds `proxy_limit` solely because of `system_instruction`, split the system instruction across multiple interactions using natural text boundaries:
   - Priority order: double newline (`\n\n`) → single newline (`\n`) → period (`.`) → exclamation/question (`!` `?`) → comma/semicolon (`,` `;`) → space
   - First chunk(s): send with the split system_instruction portion, empty `Content[]`, empty `Step[]`
   - Chain via `previous_interaction_id`
   - Last chunk: receives remaining system_instruction + the actual messages
   - This ensures long system prompts (e.g., lengthy instructions on how to format responses) don't block message delivery

**Delta accounting with splits:**
- `message_count` tracks the total number of client messages delivered across ALL chunks
- Session state stores the LAST chunk's `interaction_id` (for correct chaining)
- On subsequent requests, delta is computed against the total delivered count

#### Scenario: Messages fit in one chunk
- GIVEN `proxy_limit = "130k"`, 3 messages serialized to 50 KiB
- WHEN request is processed
- THEN single interaction sent, `message_count += 3`, stored `interaction_id` = response ID

#### Scenario: System instruction split across chunks
- GIVEN `proxy_limit = "10k"`, system_instruction is 25 KiB, messages are 2 KiB
- WHEN request is processed
- THEN chunk1 (empty Content[], system_instruction part 1 split at `\n\n`, ~9 KiB) → chunk2 (empty Content[], system_instruction part 2, ~9 KiB) → chunk3 (system_instruction part 3 + messages, ~9 KiB), `message_count += len(messages)`, stored `interaction_id` = chunk3's ID

#### Scenario: Messages split across chunks (with prior session state)
- GIVEN `proxy_limit = "15k"`, session has `interaction_id = "prior-id"` and 2 already-delivered messages. 3 new messages serialize to 25 KiB (msg1: 12 KiB, msg2: 8 KiB, msg3: 5 KiB)
- WHEN request is processed
- THEN chunk1 (msg1, 12 KiB, uses `previous_interaction_id = "prior-id"`) → chunk2 (msg2+msg3, 13 KiB, chained to chunk1), `message_count += 3`, stored `interaction_id` = chunk2's ID

#### Scenario: Single message exceeds limit
- GIVEN `proxy_limit = "1k"`, a single message serializes to 5 KiB
- WHEN request is processed
- THEN 415 error returned with body `"Unable to split ingress message into chunks under proxy limit."`, no interaction sent

### Session persistence

Sessions are persisted to a TOML file. Default paths by platform:

| Platform | Default path |
|----------|-------------|
| Linux (.deb) | `/var/lib/inf-splitter/interactions-sessions.toml` |
| Windows | `%ProgramData%\inf-splitter\interactions-sessions.toml` |

Override via global config key `interactions_session_store`. This is a runtime data file, not a config file.

The file is written atomically on every state change. On graceful shutdown (SIGTERM/SIGINT) and on panic (via a panic hook), the store is also flushed.

**Pending sessions on shutdown:** a session created but whose interaction may not have completed yet is written with a `pending = true` flag. On next startup, for each pending session the proxy calls `GET /v1beta/interactions/{id}` to verify whether the interaction was actually created on Google's side:

- **200 OK** → interaction exists, clear the pending flag (session is valid)
- **404 / error** → interaction was never created, remove the session from the store

This prevents "zombie" sessions where the TOML records an interaction ID that never materialised upstream.

**Error tolerance:** CANCEL (`POST .../cancel`) and DELETE operations may return errors (e.g. 404 "no such interaction"). These errors are logged but otherwise ignored — the goal is to clean up local state, and "already gone" is an acceptable outcome.

On startup: load TOML, clean expired sessions (DELETE + POST cancel), restore active ones.

```toml
[session_id]
interaction_id = "abc123"
message_count = 5
last_access_utc = 1718570000
expires_at_utc = 1718571800
pending = false
```

On startup, expired sessions are cleaned up (DELETE sent to their interaction IDs, then removed from file).

### Session lifecycle operations

The interactions API supports four operations on interactions:

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1beta/interactions?model` | **Create** — initiate a new interaction (primary operation) |
| `GET` | `/v1beta/interactions/{id}` | Retrieve interaction state |
| `POST` | `/v1beta/interactions/{id}/cancel` | Cancel ongoing processing |
| `DELETE` | `/v1beta/interactions/{id}` | Delete interaction (release resources) |

The primary operation (`POST ?model`) is used for all message forwarding. GET, cancel, and DELETE are used for session management and cleanup.

### In-band control messages

Clients can manage sessions by embedding specially-formatted text in messages. The proxy intercepts these **before** forwarding to the interactions endpoint. Control messages are:

1. **Clean all** — close all sessions for this endpoint:
   ```
   ***!___!--- очисти все сессии gemini interactions ---!___!***
   ```
   On receipt: iterate all sessions, POST cancel + DELETE to each, clear session file → return confirmation to client.

2. **Extend lifetime** — extend current session's TTL:
   ```
   ***!___!--- текущую сессию gemini interactions храни до 1718571800 ---!___!***
   ```
   On receipt: update `expires_at_utc` for the current session (identified by `request_id`), persist.

Control message patterns are **configurable** per provider section:
```toml
control_clean_all = "***!___!--- очисти все сессии gemini interactions ---!___!***"
control_extend_lifetime = "***!___!--- текущую сессию gemini interactions храни до <unix_utc> ---!___!***"
```

**Delta handling with control messages:**
- Control messages are stripped from the message list BEFORE delta computation
- The `message_count` in session state tracks only non-control messages
- This ensures control messages don't count toward the "delivered" count

**Idempotency:** Control messages are processed once. The proxy tracks a hash of processed control messages per session to avoid double-processing on client retransmission.

### Agent skills (Claude Code)

Two slash-commands for the Claude Code agent to generate control messages:

- `/gemini-interactions-clean-all` — generates a message containing the `control_clean_all` constant
- `/gemini-interactions-lifetime <duration>` — generates a message with `control_extend_lifetime` + computed unix timestamp (e.g., "ещё неделя" → now + 604800 seconds)

These are Claude Code custom slash commands, not code changes in the proxy itself. They simply format and insert the appropriate control constant into the next user message.

### Control constants endpoint

The proxy exposes the configured control constants on a dedicated endpoint so agent skills can discover them dynamically:

```
GET /interactions/v1/control-constants
```

Returns a JSON object keyed by section name, with each section's configured constants:

```json
{
  "gemini": {
    "clean_all": "***!___!--- очисти все сессии gemini interactions ---!___!***",
    "extend_lifetime": "***!___!--- текущую сессию gemini interactions храни до <unix_utc> ---!___!***"
  }
}
```

Sections without `endpoint_interactions` or without control constants are omitted. The agent skill fetches this at runtime: `curl http://$PROXY_HOST:$PROXY_PORT/interactions/v1/control-constants`.

### Session eviction

(Updated) Sessions are evicted based on `expires_at_utc`. Default TTL is **12 hours** from last access (configurable). On eviction: POST cancel + DELETE to the interaction, then remove from persisted store. Background task runs periodically.

## Scope

### In Scope
- `endpoint_interactions` config field
- `proxy` per-section config for explicit upstream proxy (applies to all handlers)
- `InteractionsHandler` — translation both directions (Anthropic↔Interactions, OpenAI↔Interactions)
- `SessionStore` — persistent session state (TOML file), TTL eviction, recovery on startup
- Session lifecycle: GET interaction, POST cancel, DELETE interaction
- Delta computation: send only new messages; chain via `previous_interaction_id`
- `request_id` → `session_id` (used directly, no extraction)
- In-band control messages: clean all sessions, extend session lifetime
- Control message stripping from delta (excluded from message count)
- Control message idempotency (processed once)
- Configurable control message constants per provider section
- `GET /interactions/v1/control-constants` — expose configured constants for agent skill discovery
- `x-goog-api-key`, `Content-Type`, `Api-Revision` headers (minimal set, no client headers)
- `proxy_limit` — egress message splitting when exceeding byte-size limit
- Non-streaming and streaming response handling
- Error translation (`[[error_translation]]`) support in interactions paths
- Token limits injection (where applicable)

### Out of Scope
- Multi-endpoint interactions (only one interactions endpoint per section)
- Interactions → Interactions passthrough (no `/v1beta/interactions` ingress exposed)
- Session migration / sharing across proxy instances
- Gemini-specific features: tools, function calling, code execution, safety settings
- `drop_fields` support in interactions paths (will be added separately if needed)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `schemas/interactions.openapi.json` | **New** | Committed copy of the OpenAPI schema |
| `build.rs` | Yes | Parse schema, generate Rust types into `OUT_DIR` |
| `config.rs` | Yes | Add `endpoint_interactions` to provider sections, update routing |
| `interactions.rs` | **New** | `InteractionsHandler` — translation, session management, upstream calls |
| `session.rs` | **New** | `SessionStore` — persistent state (TOML), TTL eviction, lifecycle operations |
| `control.rs` | **New** | Control message parsing, stripping, idempotency |
| `config/interactions-sessions.toml` | **New** | Runtime session data (path overridable via `interactions_session_store`) |
| `router.rs` | Yes | Add interactions dispatch to routing matrix |
| `auth.rs` | No | Not modified — interactions uses its own header builder, not `forward_request_headers` |
| `lib.rs` | Yes | Wire `InteractionsHandler` into `AppState` and `build_app()` |
| Config spec | Yes | Add `endpoint_interactions`, update routing scenarios |
| Routing spec | Yes | Add interactions dispatch scenarios |
| Protocol conversion spec | Yes | Add Anthropic↔Interactions, OpenAI↔Interactions translation |
| READMEs (×3) | Yes | Update config reference |
| `config/inf-splitter.toml.example` | Yes | Add interactions example |

## Architecture Considerations

### New pattern: Build-time code generation

`build.rs` reads `schemas/interactions.openapi.json` at compile time and generates Rust structs with serde `Deserialize`/`Serialize` derives. The generated code is written to `OUT_DIR` and included in `interactions.rs` via `include!`. This ensures the Rust types always match the upstream API schema. The schema file is committed to the repo — no network access during build.

### New pattern: Session state

This is the first stateful component in the proxy. `SessionStore` is behind `Arc<RwLock<>>` and shared across all handler clones. Session cleanup runs on a background tokio task.

### Translation without anyllm_translate

The interactions protocol has no CRATES.io library equivalent. Translation is hand-written with typed serde structs. This follows the pattern already used for `strip_adaptive_thinking()` — manual JSON manipulation where no library exists.

### Health check for non-standard endpoints

The interactions endpoint won't respond to HEAD. We probe the base URL of the endpoint with a short timeout — any TCP-level response is considered "healthy."

## Success Criteria

- [ ] `schemas/interactions.openapi.json` is committed and build.rs generates types from it
- [ ] `endpoint_interactions` parses correctly in TOML config
- [ ] `InteractionsHandler` translates Anthropic → Interactions and back
- [ ] `InteractionsHandler` translates OpenAI → Interactions and back
- [ ] `SessionStore` correctly tracks delivered messages and computes deltas
- [ ] `request_id` used directly as session key (ingress ID = internal session ID)
- [ ] `proxy_limit` splits oversized messages correctly; single-element-too-large returns error
- [ ] Delta computation correctly accounts for split interactions
- [ ] Streaming and non-streaming responses work for both ingress protocols
- [ ] `[[error_translation]]` works in interactions error paths
- [ ] Health check reports interactions endpoints
- [ ] `cargo fmt --check`, `cargo clippy --locked -- -D warnings`, `cargo test --locked` pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Session state memory leak | Medium | High | TTL-based eviction with background cleanup; configurable max sessions |
| Interactions protocol changes | Low | Medium | `Api-Revision` header pins protocol version |
| Delta computation off-by-one | Medium | High | Extensive unit tests with varying message counts |
| No anyllm_translate for interactions | High | Medium | Hand-written translation with typed structs; snapshot tests |
