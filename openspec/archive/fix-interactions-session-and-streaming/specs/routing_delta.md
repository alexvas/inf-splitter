# Delta: Request Routing (Interactions Sessions)

**Change ID:** `fix-interactions-session-and-streaming`
**Affects:** `src/router.rs`, `src/session.rs`, `src/interactions_handler.rs`

---

## MODIFIED

### Requirement: Health Check

Health checks for interactions endpoints now preserve query parameters from the configured URL. Previously the query string was stripped, causing health probes to hit a different endpoint than actual traffic.

#### Scenario: Interactions health check preserves query parameters
- GIVEN `endpoint_interactions = "https://api.example.com/v1beta/interactions?key=abc123"`
- WHEN `/health` probes the interactions endpoint
- THEN the probe URL is `https://api.example.com/v1beta/interactions?key=abc123` (query string preserved)
- AND the health check result reflects the actual endpoint used for forwarding

#### Scenario: Interactions health check without query parameters unchanged
- GIVEN `endpoint_interactions = "https://api.example.com/v1beta/interactions"` (no query string)
- WHEN `/health` probes the interactions endpoint
- THEN behavior is unchanged (no query string to preserve)

---

### Requirement: Session State Tracking

Streaming interactions requests now keep `pending = true` on the session until the upstream stream completes successfully. Previously `message_count` was advanced and `pending` cleared before the stream finished, so a mid-stream crash left the session in an unrecoverable state.

**Updated session state fields:** `interaction_id`, `message_count`, `last_access_utc`, `expires_at_utc`, `pending`.

**Pending semantics:**
- Set to `true` when a request begins processing (before the upstream call)
- Cleared to `false` and `message_count`/`interaction_id` updated only after the upstream response is fully received and validated
- For streaming: updated after the stream completes (all SSE events received, `interaction.completed` processed)
- For non-streaming: updated after the full JSON response is parsed

#### Scenario: Streaming session stays pending until stream completes
- GIVEN a session with `pending = false, message_count = 3`
- WHEN a streaming interactions request with 2 new messages is sent
- THEN `pending` is set to `true` before the upstream call (message_count stays 3)
- AND after the stream completes successfully, `pending` is cleared to `false` and `message_count` becomes 5

#### Scenario: Mid-stream crash leaves session pending
- GIVEN a streaming request is in progress (`pending = true`)
- WHEN the process crashes mid-stream
- THEN the persisted session file shows `pending = true` with the pre-request `message_count` and `interaction_id`
- AND on restart, the pending session is verified via `get_interaction`

---

### Requirement: Session Persistence

Session persistence now includes two additional behaviors:

**Startup pending recovery:** In `build_app`, after loading the session store, the proxy iterates all `pending_sessions()` and verifies each via `GET /v1beta/interactions/{id}`:
- If the interaction exists and is completed: clear `pending`, update `message_count` from the interaction's delivered message count
- If the interaction is still in-progress: keep `pending = true` (recovery is indeterminate; the client should re-send)
- If the interaction is not found (404): remove the session from the store and cancel/delete the stale interaction

**Periodic expired session eviction:** `SessionStore::evict_expired()` is called on each new session creation (or on a timer). Expired sessions have their upstream interactions cancelled and deleted, then are removed from the store and persisted file.

#### Scenario: Pending session recovered on startup (interaction completed)
- GIVEN persisted session with `pending = true, interaction_id = "abc123", message_count = 3`
- WHEN proxy starts and `GET /v1beta/interactions/abc123` returns a completed interaction with 5 messages
- THEN `pending` is cleared to `false`
- AND `message_count` is updated to 5
- AND the session is preserved in the store

#### Scenario: Pending session cleaned on startup (interaction gone)
- GIVEN persisted session with `pending = true, interaction_id = "abc123"`
- WHEN proxy starts and `GET /v1beta/interactions/abc123` returns 404
- THEN the session is removed from the store
- AND `cancel_interaction` + `delete_interaction` are NOT called (interaction already gone)
- AND a warning is logged

#### Scenario: Expired sessions evicted during normal operation
- GIVEN a session with `expires_at_utc` in the past
- WHEN a new session is created (or eviction timer fires)
- THEN the expired session's interaction is cancelled and deleted
- AND the session is removed from the store and persisted file
- AND cleanup errors (404, etc.) are logged but do not prevent eviction

#### Scenario: Cleanup errors are tolerated (unchanged)
- GIVEN eviction triggers DELETE for `interaction_id = "abc123"`
- WHEN upstream returns 404 "no such interaction"
- THEN error is logged, session removed from local store

---

### Requirement: Interactions Session Response Header

Split-path error responses now include the session identifier header. Previously `handle_split_send` and `send_split_system_instruction` error paths returned responses without `x-claude-code-session-id` or `x-request-id`.

Updated covered response paths:
- `send_and_translate` — non-streaming success and error
- `handle_stream_response` — streaming (via `sse_response_with_extra_header`)
- `handle_split_send` — split-send chunk responses (success and **error**)
- `send_split_system_instruction` — system-instruction split responses (success and **error**)
- Control message responses (clean-all, extend-lifetime)

#### Scenario: Split-send error carries session header
- GIVEN a split-send request where chunk 2 returns an upstream error
- WHEN `handle_split_send` returns the translated error response
- THEN the response includes the session header (`x-claude-code-session-id` or `x-request-id`)

#### Scenario: System-instruction split error carries session header
- GIVEN `send_split_system_instruction` fails on an upstream error
- WHEN the error response is returned
- THEN the response includes the session header

---

### Requirement: cancel_interaction and delete_interaction Check HTTP Status

`cancel_interaction` and `delete_interaction` now check the HTTP status code from the upstream response. On non-2xx status codes, the error is logged and returned to the caller (instead of silently treating it as success).

#### Scenario: cancel_interaction returns HTTP 500
- GIVEN `cancel_interaction("abc123")` sends POST to upstream
- WHEN upstream returns HTTP 500
- THEN the function returns `Err(...)` with the status code and error body
- AND `tracing::warn!` is emitted
- AND the caller treats the cancellation as failed

#### Scenario: delete_interaction returns HTTP 500
- GIVEN `delete_interaction("abc123")` sends DELETE to upstream
- WHEN upstream returns HTTP 500
- THEN the function returns `Err(...)` with the status code and error body

#### Scenario: Successful cancel/delete unchanged
- GIVEN upstream returns HTTP 200 for cancel or 204 for delete
- WHEN the function processes the response
- THEN `Ok(())` is returned (unchanged behavior for success)

---

## ADDED

(None)

---

## REMOVED

(None)
