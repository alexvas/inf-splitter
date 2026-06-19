# Delta: Request Routing

**Change ID:** `add-interactions-protocol`
**Affects:** `src/router.rs`, `src/lib.rs`

---

## ADDED

### Requirement: Interactions Dispatch

When a model resolves to a section with `endpoint_interactions` set, requests are routed to `InteractionsHandler`:

| Ingress | Interactions endpoint set | Action |
|---------|--------------------------|--------|
| OpenAI | Yes | `InteractionsHandler::handle_from_openai()` |
| Anthropic | Yes | `InteractionsHandler::handle_from_anthropic()` |

When both `endpoint_interactions` and another endpoint are set, the ingress protocol matching its direct endpoint takes priority.

#### Scenario: Anthropic ingress → Interactions
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/messages` arrives
- THEN the request is translated Anthropic→Interactions and sent upstream

#### Scenario: OpenAI ingress → Interactions
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/chat/completions` arrives
- THEN the request is translated OpenAI→Interactions and sent upstream

#### Scenario: Both endpoints — ingress preference
- GIVEN section has `endpoint_interactions` and `endpoint_anthropic`
- WHEN `POST /v1/messages` arrives
- THEN passthrough to `endpoint_anthropic` (prefers matching protocol)
- WHEN `POST /v1/chat/completions` arrives
- THEN translates to interactions (OpenAI endpoint not available)

### Requirement: Session State Tracking

`InteractionsHandler` uses a `SessionStore` to track session state. The Interactions API maintains conversation state through `previous_interaction_id` chaining.

**Session state struct:**
```rust
SessionState {
    interaction_id: String,   // from the last successful Interaction.id
    message_count: usize,      // total client messages delivered (adjusted for controls and splits)
    last_access_utc: u64,      // for TTL eviction
    expires_at_utc: u64,       // configurable expiry (default +12h)
    pending: bool,             // true if interaction creation may not have completed yet
}
```

**Pending sessions:** on shutdown, sessions are written with `pending = true`. On startup, for each pending session the proxy calls `GET /v1beta/interactions/{id}`:
- 200 OK → interaction exists, set `pending = false` (session is valid)
- 404 / error → interaction was never created, remove session from store

**Error tolerance for cleanup:** CANCEL and DELETE operations that return errors (404 "no such interaction", etc.) are logged and then ignored — "already gone" is an acceptable outcome.

**`message_count` accounting rules:**

| Situation | How `message_count` changes |
|-----------|---------------------------|
| Normal request with N messages | `+= N` |
| Control messages present | Control messages are subtracted before counting (excluded from N) |
| `proxy_limit` split into K chunks | `+= sum(messages_in_each_chunk)` — tracks total across all chunks |

The stored `interaction_id` is always the LAST successful interaction's ID (after all splits), ensuring correct chaining on subsequent requests.

**Session ID** — determined in priority order:

1. **HTTP header `x-request-id`** (primary) — de-facto standard, many clients set this as request header
2. **Body field `request_id`** (fallback) — inf-splitter's diagnostic field
3. **Random UUID** (last resort) — no multi-turn optimisation but functional

#### Scenario: First request in session
- GIVEN no prior session for `request_id = "1781741490-0"`

#### Scenario: First request in session
- GIVEN no prior messages for session `"1781741490"`
- WHEN request with 3 messages arrives
- THEN all 3 messages are translated and sent

#### Scenario: Subsequent request — delta
- GIVEN session `"1781741490"` has 3 delivered messages
- WHEN request with 5 messages arrives (same session)
- THEN only messages [3..5] (2 messages) are sent

#### Scenario: No new messages
- GIVEN session `"1781741490"` has 5 delivered messages
- WHEN request with 5 messages arrives (same count)
- THEN empty delta — request is forwarded with empty input (or receives immediate cached response)

#### Scenario: Session ID from HTTP header (primary)
- GIVEN `x-request-id: conv-abc123` header and `request_id: "1781741490-5"` in body
- WHEN session is resolved
- THEN `session_id = "conv-abc123"` (header wins over body field)

#### Scenario: Fallback to body field
- GIVEN no `x-request-id` header, `request_id: "1781741490-5"` in body
- WHEN session is resolved
- THEN `session_id = "1781741490-5"`

#### Scenario: Random UUID fallback
- GIVEN neither `x-request-id` header nor `request_id` body field
- WHEN session is resolved
- THEN a random UUID is generated as session ID, request processed normally (no multi-turn delta since prior messages unknown)

### Requirement: Session Persistence

Session state is persisted to a TOML file. The file is:

- Written atomically on every state change (new session, updated message count, extended TTL)
- Flushed on graceful shutdown (SIGTERM/SIGINT) and via panic hook — no state loss on unexpected termination
- Read on startup to recover sessions from previous run
- Cleaned on startup: expired sessions have POST cancel and DELETE sent before removal; pending sessions verified via GET (200→keep, 404→remove)

#### Scenario: Session survives restart
- GIVEN session `"1781741490"` with `interaction_id = "abc123"` and `message_count = 5`
- WHEN proxy restarts
- THEN session is recovered from TOML, `message_count` is 5, `interaction_id` is "abc123"

#### Scenario: Expired sessions cleaned on startup
- GIVEN TOML file has an expired session (last_access + TTL in the past)
- WHEN proxy starts
- THEN POST cancel + DELETE are sent to the interaction ID, session removed from file

#### Scenario: Pending session verified on startup
- GIVEN TOML file has a session with `pending = true`
- WHEN proxy starts and `GET /v1beta/interactions/{id}` returns 200
- THEN `pending` is cleared to `false`, session is kept

#### Scenario: Pending session removed if not created
- GIVEN TOML file has a session with `pending = true`
- WHEN proxy starts and `GET /v1beta/interactions/{id}` returns 404
- THEN session is removed from the store

#### Scenario: Cleanup errors are tolerated
- GIVEN eviction or clean-all triggers DELETE for `interaction_id = "abc123"`
- WHEN upstream returns 404 "no such interaction"
- THEN the error is logged, session is removed from local store regardless

#### Scenario: Graceful shutdown flushes state
- GIVEN active sessions in memory
- WHEN proxy receives SIGTERM
- THEN all sessions are written to TOML before process exits

#### Scenario: Panic flushes state
- GIVEN active sessions in memory
- WHEN a panic occurs
- THEN panic hook writes all sessions to TOML before abort

### Requirement: Session Lifecycle Operations

`SessionStore` supports four operations on interactions using the configured `api_key` and headers:

| Operation | HTTP | Endpoint | Purpose |
|-----------|------|----------|---------|
| Create | `POST` | `/v1beta/interactions?model` | Initiate a new interaction (primary — used for all message forwarding) |
| Get | `GET` | `/v1beta/interactions/{id}` | Retrieve interaction state |
| Cancel | `POST` | `/v1beta/interactions/{id}/cancel` | Cancel ongoing LLM processing |
| Delete | `DELETE` | `/v1beta/interactions/{id}` | Release server resources |

#### Scenario: Cancel on eviction
- GIVEN session expires
- WHEN cleanup runs
- THEN `POST /v1beta/interactions/{id}/cancel` is sent before `DELETE`

### Requirement: In-Band Control Messages

Clients can manage sessions by embedding special text constants in conversation messages. The proxy detects these constants **before** forwarding to the interactions endpoint and handles them locally.

**Configuration** (per provider section):
```toml
control_clean_all = "***!___!--- очисти все сессии gemini interactions ---!___!***"
control_extend_lifetime = "***!___!--- текущую сессию gemini interactions храни до <unix_utc> ---!___!***"
```

**Processing rules:**
1. Control messages are stripped from the message list before delta computation
2. `message_count` in session state tracks only non-control messages
3. Control messages are processed once (idempotent via hash tracking)

#### Scenario: Clean all sessions
- GIVEN 3 active sessions exist
- WHEN client sends a message containing `control_clean_all`
- THEN all 3 sessions are cancelled (POST cancel) and deleted (DELETE), session file is emptied

#### Scenario: Extend session lifetime
- GIVEN current session expires at `1718570000`
- WHEN client sends a message containing `control_extend_lifetime` with timestamp `1719174800`
- THEN session's `expires_at_utc` is updated to `1719174800`, persisted to TOML

#### Scenario: Control message stripped from delta
- GIVEN 3 delivered messages, new request has 2 messages: [text, control_extend_lifetime, text]
- WHEN computing delta
- THEN control message is excluded, delta = 2 messages (only the text ones), `message_count` becomes 5

#### Scenario: Control message idempotency
- GIVEN control message `clean_all` was processed
- WHEN client re-sends the same control message (retransmission)
- THEN it is ignored (hash matches already-processed set)

### Requirement: Control Constants HTTP Endpoint

The proxy exposes configured control constants on a dedicated endpoint so agent skills can discover them dynamically at runtime.

```
GET /interactions/v1/control-constants
```

Returns JSON keyed by section name. Only sections with `endpoint_interactions` AND at least one control constant are included.

#### Scenario: Constants returned
- GIVEN config has `[gemini]` with `endpoint_interactions`, `control_clean_all = "***!___!--- очисти все сессии gemini interactions ---!___!***"`, `control_extend_lifetime = "..."`
- WHEN `GET /interactions/v1/control-constants` is called
- THEN response is `{"gemini": {"clean_all": "...", "extend_lifetime": "..."}}`

#### Scenario: Sections without control constants omitted
- GIVEN config has `[gemini]` with `endpoint_interactions` but no control constants configured
- WHEN endpoint is called
- THEN `gemini` is absent from response

#### Scenario: Non-interactions sections omitted
- GIVEN config has sections without `endpoint_interactions`
- WHEN endpoint is called
- THEN those sections are not included

### Requirement: Egress Message Splitting (proxy_limit)

When `proxy_limit` is configured on a provider section, the serialized `Content[]` is measured before sending. If it exceeds the limit, the array is split into multiple interactions, chained via `previous_interaction_id`.

**Splitting algorithm:**
1. Serialize `Content[]` to JSON bytes
2. If ≤ limit → send single interaction
3. If > limit → **sequential greedy packing**: iterate `Content` elements in order, add to current chunk if it fits under the limit, otherwise start a new chunk. Message order is preserved — no reordering for "optimal" packing.
4. First chunk uses session's existing `interaction_id` as `previous_interaction_id` (if any)
5. Subsequent chunks chain: chunk N+1 uses chunk N's `interaction.id` as `previous_interaction_id`
6. Store the LAST chunk's `interaction.id` in session state
7. `message_count` increased by total messages across all chunks

**Error case 1:** If a single `Content` element serialized alone exceeds `proxy_limit`, return 415 with body `"Unable to split ingress message into chunks under proxy limit."`.

**Error case 2 (system instruction splitting):** If the request (even with empty `Content[]`) exceeds `proxy_limit` solely due to `system_instruction`, split the instruction across chunks rather than failing. Split priority: `\n\n` → `\n` → `.` → `!`/`?` → `,`/`;` → space. First chunk(s) carry the split instruction + empty `Content[]`; the last chunk carries the remaining instruction + actual messages. Chain all via `previous_interaction_id`.

#### Scenario: Single interaction (under limit)
- GIVEN `proxy_limit = "130k"`, 3 messages serialized to 50 KiB
- WHEN request is processed
- THEN single interaction sent, `message_count += 3`

#### Scenario: Split across multiple interactions (with prior session)
- GIVEN `proxy_limit = "15k"`, session has `interaction_id = "prior-id"` and 2 already-delivered messages. 3 new messages (msg1: 6 KiB, msg2: 6 KiB, msg3: 4 KiB)
- WHEN request is processed
- THEN chunk1 (msg1+msg2, 12 KiB, `previous_interaction_id = "prior-id"`) → chunk2 (msg3, 4 KiB, chained to chunk1)
- THEN session stores chunk2's `interaction_id`, `message_count += 3`

#### Scenario: Сhunks preserve message order (sequential greedy)
- GIVEN `proxy_limit = "10k"`, 3 messages (msg1: 6 KiB, msg2: 5 KiB, msg3: 4 KiB)
- WHEN request is processed
- THEN chunk1 = [msg1] (6 KiB, msg2 doesn't fit: 6+5=11 > 10) → chunk2 = [msg2, msg3] (5+4=9 KiB). Order preserved — no reordering.

#### Scenario: Single element exceeds limit
- GIVEN `proxy_limit = "1k"`, a single message serializes to 5 KiB
- WHEN request is processed
- THEN 415 error returned with body `"Unable to split ingress message into chunks under proxy limit."`, no interactions created

---

## MODIFIED

### Requirement: Request Dispatch

The routing matrix now includes interactions. The dispatch function checks:
1. If `endpoint_openai` is set → `OpenAiHandler` (as before)
2. If `endpoint_anthropic` is set → `AnthropicHandler` (as before)
3. If `endpoint_interactions` is set → `InteractionsHandler` (new)

(Updated dispatch logic to consider three endpoint types instead of two.)

### Requirement: Health Check

Health probes now also check interactions endpoints. The interactions endpoint is probed with GET (not HEAD) since it may not support HEAD.

---

## REMOVED

(None)
