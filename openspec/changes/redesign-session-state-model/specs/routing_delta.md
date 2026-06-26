# Delta: Routing

**Change ID:** `redesign-session-state-model`
**Affects:** `openspec/specs/routing.md`, `src/router.rs`, `src/lib.rs`, `src/interactions_handler.rs`, `src/session.rs`

---

## MODIFIED

### Requirement: Session State Tracking

`InteractionsHandler` no longer tracks delivered messages by raw `message_count`. Conversation frontier is derived from canonical harness-message hashes and `InteractionStore`.

Session ID resolution remains unchanged:
1. HTTP header `x-request-id`
2. HTTP header `x-claude-code-session-id`
3. Body field `request_id`
4. Random UUID v7

Resolved session ID is used for response headers, metadata, in-flight batch matching, control-message scope, and diagnostics. It MUST NOT by itself select `previous_interaction_id`.

#### Scenario: First request in session
- GIVEN no matching hash prefix in `InteractionStore`
- WHEN a request with 3 harness messages arrives
- THEN all 3 harness messages are translated and sent
- AND no `previous_interaction_id` is sent

#### Scenario: Subsequent request — hash prefix delta
- GIVEN `InteractionStore` has a valid chain for harness hashes `[0xA, 0xB, 0xC]` ending at `int-3`
- WHEN incoming harness hashes are `[0xA, 0xB, 0xC, 0xD, 0xE]`
- THEN only messages `[0xD, 0xE]` are sent
- AND `previous_interaction_id = "int-3"`

#### Scenario: History rewrite with same count
- GIVEN session metadata exists for `sess-1`
- AND incoming harness hashes differ from any known prefix
- WHEN the request contains the same number of messages as a prior request
- THEN the proxy starts a new chain instead of reusing stale `previous_interaction_id`

#### Scenario: Session ID from HTTP header still wins
- GIVEN `x-request-id: conv-abc123` header and `request_id: "body-id"` in body
- WHEN session is resolved
- THEN `session_id = "conv-abc123"`

### Requirement: Session Persistence

Session persistence changes from old count-based `SessionState` to a versioned v2 document containing sessions, interactions, and in-flight batches. The configured `interactions_session_store` path remains the storage path.

Startup recovery:
- Load v2 document atomically.
- Ignore old v1 count-based files with a warning.
- Rebuild `hash_index` from persisted `InteractionNode.message_hashes`.
- For each in-flight batch:
  - `Acked` pieces are trusted.
  - `Sent` pieces are verified by stored interaction id when available; otherwise they are treated as indeterminate and failed with a retryable error.
  - `Pending` pieces are resent from persisted request data.
  - complete batches insert their terminal `InteractionNode`.
  - failed/expired batches are retained until TTL eviction or removed by clean-all.

Periodic eviction MUST remove expired `SessionInfo`, `InteractionNode`, stale `hash_index` positions, and `InFlightBatch` entries. Upstream deletion remains best-effort.

#### Scenario: V2 store survives restart
- GIVEN persistence file has `version = 2` with one session, one interaction node, and no in-flight batches
- WHEN proxy starts
- THEN all stores are loaded
- AND `hash_index` lookups work for the loaded node hashes

#### Scenario: Old store resets safely
- GIVEN persistence file has old `interaction_id`, `message_count`, and `pending` entries
- WHEN proxy starts
- THEN old entries are ignored
- AND no stale `previous_interaction_id` can be selected from old counts

#### Scenario: In-flight batch recovered
- GIVEN persisted batch has P0 Acked and P1 Pending with persisted request body
- WHEN proxy starts
- THEN P0 is not resent
- AND P1 is resent with `previous_interaction_id` from P0

### Requirement: Interactions Session Response Header

All `InteractionsHandler` response paths still return the resolved session ID as response header:
- Anthropic ingress -> `x-claude-code-session-id: <session_id>`
- OpenAI ingress -> `x-request-id: <session_id>`

This header is independent of `InteractionStore` frontier selection.

#### Scenario: Replay response includes session header
- GIVEN all incoming harness hashes are known and replay is used
- WHEN response is returned to an Anthropic client
- THEN response includes `x-claude-code-session-id: <session_id>`

### Requirement: In-Band Control Messages

Control messages MUST be stripped before harness-message filtering and hashing.

Clean-all MUST:
- cancel/delete known terminal interactions best-effort;
- cancel ACKed in-flight piece interactions best-effort;
- clear `SessionInfo`, `InteractionStore`, `hash_index`, and `InFlightStore`.

Extend-lifetime MUST update current `SessionInfo` and the matched current `InteractionNode` when one exists. It MUST NOT create a routing dependency on `SessionInfo.last_interaction_id`.

Control-message idempotency remains hash-based and excludes control messages from harness hash frontier.

#### Scenario: Control excluded from hash frontier
- GIVEN request contains two harness messages and one control message
- WHEN chain frontier is computed
- THEN only two harness messages participate in hash prefix matching

#### Scenario: Clean-all clears all stores
- GIVEN sessions, interaction nodes, hash-index entries, and in-flight batches exist
- WHEN clean-all control is processed
- THEN all local stores are empty after best-effort upstream cleanup
