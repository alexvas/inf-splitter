# Spec: Request Routing

Components: `src/router.rs`, `src/lib.rs`, `src/auth.rs`, `src/error.rs`

## Requirement: HTTP API Endpoints

The proxy exposes these routes:

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/health` | Readiness probe |
| `GET` | `/v1/models` | Model list (redirects to `/openai/v1/models`) |
| `GET` | `/openai/v1/models` | OpenAI-format model list |
| `GET` | `/anthropic/v1/models` | Anthropic-format model list |
| `POST` | `/v1/chat/completions` | OpenAI chat completions ingress |
| `POST` | `/v1/messages` | Anthropic messages ingress |
| `GET` | `/interactions/v1/control-constants` | Control message constants for interactions sections |

### Scenario: Unknown route
- GIVEN any unrecognized path
- WHEN a request arrives
- THEN the proxy returns 404

## Requirement: Request Dispatch

On receiving a POST body, the router:
1. Peeks the `model` field from JSON (without consuming the body)
2. Resolves the model to a `RouteTarget` via config
3. Determines the handler based on ingress protocol and available endpoints

### Scenario: OpenAI ingress with OpenAI endpoint
- GIVEN section has `endpoint_openai` set
- WHEN `POST /v1/chat/completions` arrives with a matching model
- THEN `OpenAiHandler` sends passthrough to the OpenAI upstream

### Scenario: OpenAI ingress with only Anthropic endpoint
- GIVEN section has only `endpoint_anthropic` set
- WHEN `POST /v1/chat/completions` arrives
- THEN `AnthropicHandler` translates OpenAI→Anthropic, calls upstream, translates response back

### Scenario: Anthropic ingress with Anthropic endpoint
- GIVEN section has `endpoint_anthropic` set
- WHEN `POST /v1/messages` arrives with a matching model
- THEN `AnthropicHandler` sends passthrough to the Anthropic upstream

### Scenario: Anthropic ingress with only OpenAI endpoint
- GIVEN section has only `endpoint_openai` set
- WHEN `POST /v1/messages` arrives
- THEN `OpenAiHandler` translates Anthropic→OpenAI, calls upstream, translates response back

## Requirement: Health Check

`GET /health` probes each unique upstream endpoint:
- HEAD request for OpenAI/Anthropic endpoints (strips query params, appends "/")
- GET request for interactions endpoints (uses the configured endpoint as-is, preserving query parameters)
- 2-second timeout per upstream check
- 5-second result cache
- Parallel checks for all endpoints

### Scenario: All upstreams healthy
- GIVEN all upstream endpoints respond to HEAD requests
- WHEN `/health` is called
- THEN returns `{"status":"ok","upstreams":{...}}` with HTTP 200

### Scenario: One upstream unhealthy
- GIVEN at least one upstream is unreachable
- WHEN `/health` is called
- THEN returns `{"status":"degraded","upstreams":{...}}` with HTTP 503

### Scenario: Cache hit
- GIVEN a health check was performed less than 5 seconds ago
- WHEN `/health` is called again
- THEN the cached result is returned without new probes

## Requirement: Model List

`GET /v1/models` (and protocol-specific variants) returns all explicitly listed model IDs from the config, in lexicographic order. Models listed as `"default"` are excluded.

### Scenario: Model list
- GIVEN config has models `["deepseek-v4-pro", "gemma4:31b", "default"]`
- WHEN `/v1/models` is called
- THEN returns `["deepseek-v4-pro", "gemma4:31b"]` in JSON array

## Requirement: Body Size Limit

The proxy enforces `max_request_body` via `tower-http::limit::RequestBodyLimitLayer`. On 413 errors, a JSON error is returned: `{"type":"error","error":{"type":"invalid_request_error","message":"Request body exceeds limit."}}`.

### Scenario: Body exceeds limit
- GIVEN `max_request_body = "1m"`
- WHEN a request body exceeds 1 MiB
- THEN the proxy returns 413 with a JSON error

## Requirement: Upstream Error Body Translation

When an upstream returns a non-success HTTP status with a non-streaming response, the error body is checked against the configured `[[error_translation]]` rules. On match, the body is replaced with the rule's `egress` string. On no match, the body passes through unchanged.

The `apply_error_translation(status, body, rules) -> String` function in `lib.rs` implements the matching logic — iterating rules in order, returning the translated body on first match, or the original body if no rule matches.

Translation applies to all four routing directions (openai→openai, openai→anthropic, anthropic→anthropic, anthropic→openai) and both streaming and non-streaming error paths.

### Scenario: Upstream 413 error translated
- GIVEN `[[error_translation]]` has `{status = 413, egress = "body too large"}`
- WHEN upstream returns 413 with any body
- THEN the client receives 413 with body `"body too large"`

### Scenario: Upstream error passes through
- GIVEN no matching translation rule for status 500
- WHEN upstream returns 500 with body `"internal error"`
- THEN the client receives 500 with body `"internal error"`

### Scenario: Substring match required
- GIVEN `{status = 413, ingress = "vague", egress = "translated"}`
- WHEN upstream returns 413 with body `"some other error"`
- THEN body passes through unchanged (substring does not match)

## Requirement: Auth Header Forwarding

`forward_request_headers()` copies non-hop-by-hop headers from the client request to the upstream request. If the section has `api_key` set, the `Authorization` header is overridden with the configured key.

### Scenario: API key override
- GIVEN section has `api_key = "sk-..."`
- WHEN forwarding headers to upstream
- THEN `Authorization: Bearer sk-...` is set regardless of client's auth header

### Scenario: Client auth passthrough
- GIVEN section has no `api_key`
- WHEN forwarding headers to upstream
- THEN the client's `Authorization` header is passed through unchanged

## Requirement: Session Identifier Header Mapping (Egress)

`forward_request_headers_map()` in `auth.rs` adds complementary session identifier headers when forwarding to upstreams. If one header is present and the other is absent, the missing header is inserted with the same value.

### Scenario: Anthropic client → adds x-request-id for OpenAI upstream
- GIVEN client sends `x-claude-code-session-id: abc123` but no `x-request-id`
- WHEN headers are forwarded
- THEN both `x-claude-code-session-id: abc123` and `x-request-id: abc123` are present

### Scenario: OpenAI client → adds x-claude-code-session-id for Anthropic upstream
- GIVEN client sends `x-request-id: def456` but no `x-claude-code-session-id`
- WHEN headers are forwarded
- THEN both `x-request-id: def456` and `x-claude-code-session-id: def456` are present

### Scenario: Both headers present — no overwrite
- GIVEN client sends both `x-request-id: req-789` and `x-claude-code-session-id: sess-789`
- WHEN headers are forwarded
- THEN each keeps its original value unchanged

## Requirement: Error Format

All error responses follow the Anthropic API error shape: `{"type":"error","error":{"type":"...","message":"..."}}`.

### Scenario: Upstream error
- GIVEN an upstream returns an error response
- WHEN the proxy relays it to the client
- THEN it uses the Anthropic error JSON format

## Requirement: Interactions Dispatch

When a model resolves to a section with `endpoint_interactions` set, requests are routed to `InteractionsHandler`:

| Ingress | Interactions endpoint set | Action |
|---------|--------------------------|--------|
| OpenAI | Yes | `InteractionsHandler::handle_from_openai()` → returns OpenAI format |
| Anthropic | Yes | `InteractionsHandler::handle_from_anthropic()` → returns Anthropic format |

When both `endpoint_interactions` and another endpoint are set, the ingress protocol matching its direct endpoint takes priority.

### Scenario: Anthropic ingress → Interactions
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/messages` arrives
- THEN the request is translated Anthropic→Interactions and sent upstream

### Scenario: OpenAI ingress → Interactions
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/chat/completions` arrives
- THEN the request is translated OpenAI→Interactions and sent upstream

### Scenario: Client auth headers suppressed for interactions
- GIVEN section has `api_key` set and client sends `Authorization: Bearer sk-ant-...`
- WHEN the request is forwarded to the interactions upstream
- THEN client auth headers (`Authorization`, `x-api-key`) are stripped, only `x-goog-api-key` is sent

### Scenario: No API key — client auth passthrough
- GIVEN section has no `api_key` configured
- WHEN the request is forwarded to the interactions upstream
- THEN client auth headers (if any) pass through unchanged

### Scenario: Non-auth headers always forwarded
- GIVEN `api_key` is set and client sends `x-request-id: trace-123`
- WHEN the request is forwarded to the interactions upstream
- THEN `x-request-id` is present regardless of `api_key`

### Scenario: x-goog-api-key matches configured api_key
- GIVEN section has `api_key = "my-gemini-key"`
- WHEN `POST /v1/messages` is dispatched to interactions upstream
- THEN upstream receives `x-goog-api-key: my-gemini-key`

## Requirement: Response Translation to Client Protocol

When `InteractionsHandler` receives an upstream response, it translates the response back to the client's ingress protocol:

| Ingress | Non-streaming response | Streaming response |
|---------|----------------------|--------------------|
| Anthropic | `MessageResponse` JSON (`{"type":"message","role":"assistant",...}`) | `StreamEvent` SSE (`event: content_block_delta\n...`) |
| OpenAI | `ChatCompletionResponse` JSON (`{"object":"chat.completion","choices":[...],...}`) | `ChatCompletionChunk` SSE (`data: {json}\n\ndata: [DONE]\n\n`) |

### Scenario: Anthropic ingress → Interactions → Anthropic response (non-streaming)
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/messages` arrives without `stream: true`
- THEN the upstream `Interaction` is translated to an Anthropic `MessageResponse` JSON

### Scenario: OpenAI ingress → Interactions → OpenAI response (non-streaming)
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/chat/completions` arrives without `stream: true`
- THEN the upstream `Interaction` is translated to an OpenAI `ChatCompletionResponse` JSON

### Scenario: Anthropic ingress → Interactions → Anthropic SSE (streaming)
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/messages` arrives with `stream: true`
- THEN upstream SSE events are translated to Anthropic `StreamEvent` SSE

### Scenario: OpenAI ingress → Interactions → OpenAI SSE (streaming)
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/chat/completions` arrives with `stream: true`
- THEN upstream SSE events are converted to OpenAI `ChatCompletionChunk` SSE via `ReverseStreamingTranslator`

## Requirement: Session State Tracking

`InteractionsHandler` uses a `SessionStore` to track session state. The Interactions API maintains conversation state through `previous_interaction_id` chaining.

Session state fields: `interaction_id`, `message_count`, `last_access_utc`, `expires_at_utc`, `pending`.

**Session ID resolution** (priority order):
1. HTTP header `x-request-id` (primary)
2. HTTP header `x-claude-code-session-id` (Claude CLI)
3. Body field `request_id` (fallback)
4. Random UUID v7 (last resort)

### Scenario: First request in session
- GIVEN no prior messages for session
- WHEN request with 3 messages arrives
- THEN all 3 messages are translated and sent

### Scenario: Subsequent request — delta
- GIVEN session has 3 delivered messages
- WHEN request with 5 messages arrives (same session)
- THEN only messages [3..5] are sent

### Scenario: Session ID from HTTP header (primary)
- GIVEN `x-request-id: conv-abc123` header and `request_id: "..."` in body
- WHEN session is resolved
- THEN `session_id = "conv-abc123"` (header wins)

### Scenario: Claude CLI session ID
- GIVEN `x-claude-code-session-id: 1b9db61a-154f-45ba-827c-6f898f4cf831` header and no `x-request-id`
- WHEN session is resolved
- THEN `session_id = "1b9db61a-154f-45ba-827c-6f898f4cf831"`

### Scenario: x-request-id still wins over x-claude-code-session-id
- GIVEN both `x-request-id: req-123` and `x-claude-code-session-id: session-456`
- WHEN session is resolved
- THEN `session_id = "req-123"` (x-request-id wins)

## Requirement: Session Persistence

Session state is persisted to a TOML file. Written atomically on every state change, flushed on shutdown/panic.

**Startup recovery:** In `build_app`, after loading the session store, the proxy iterates all `pending_sessions()` and verifies each via `GET /v1beta/interactions/{id}` against each configured interactions endpoint:
- If the interaction exists (200): clear `pending`, keep the session
- If the interaction is not found (404): remove the session from the store
- If the interaction is still in-progress: keep `pending = true` (recovery is indeterminate)
- Errors during verification are logged; the session stays pending for a future recovery attempt

**Periodic eviction:** On each new session creation (`get_or_create`), expired sessions (where `now > expires_at_utc`) are evicted from the store and the persisted file. The upstream interaction is not explicitly cancelled/deleted during periodic eviction — the upstream's own TTL handles cleanup.

**Streaming pending:** Streaming requests set `pending = true` before the upstream call and advance `message_count` eagerly (to prevent racing follow-up requests from re-sending in-flight messages). `pending` is cleared to `false` only after the stream completes successfully and the real `interaction_id` is known. On crash, the pending flag triggers startup verification.

### Scenario: Session survives restart
- GIVEN session with `interaction_id = "abc123"` and `message_count = 5`
- WHEN proxy restarts
- THEN session is recovered from TOML with same state

### Scenario: Expired sessions evicted on new session creation
- GIVEN an expired session in the store
- WHEN a new session is created via `get_or_create`
- THEN the expired session is removed from the store and persisted file
- AND `tracing::info!` logs the eviction count

### Scenario: Pending session verified on startup
- GIVEN session with `pending = true`
- WHEN proxy starts and `GET /v1beta/interactions/{id}` returns 200
- THEN `pending` cleared to `false`, session kept

### Scenario: Pending session not found on startup
- GIVEN session with `pending = true`
- WHEN proxy starts and all interactions endpoints return 404 for the interaction
- THEN the session is removed from the store
- AND `cancel_interaction`/`delete_interaction` are not called (interaction already gone)

### Scenario: Cleanup errors are tolerated
- GIVEN eviction triggers DELETE for `interaction_id = "abc123"`
- WHEN upstream returns 404 "no such interaction"
- THEN error is logged, session removed from local store

## Requirement: Interactions Session Response Header

All `InteractionsHandler` response paths return the resolved session ID as a response header. The header name depends on ingress protocol:
- Anthropic ingress → `x-claude-code-session-id: <session_id>`
- OpenAI ingress → `x-request-id: <session_id>`

The header is set via `session_header_name(ingress)` which maps `Protocol::Anthropic` → `"x-claude-code-session-id"`, `Protocol::OpenAi` → `"x-request-id"`.

Covered response paths:
- `send_and_translate` — non-streaming success and error
- `handle_stream_response` — streaming (via `sse_response_with_extra_header`)
- `handle_split_send` — split-send chunk responses
- Control message responses (clean-all, extend-lifetime)

### Scenario: Anthropic client gets x-claude-code-session-id
- GIVEN an Anthropic ingress request with `x-claude-code-session-id: abc`
- WHEN the Interactions response is returned
- THEN response includes header `x-claude-code-session-id: abc`

### Scenario: OpenAI client gets x-request-id
- GIVEN an OpenAI ingress request with `x-request-id: def`
- WHEN the Interactions response is returned
- THEN response includes header `x-request-id: def`

## Requirement: In-Band Control Messages

Clients can manage sessions by embedding special text constants in conversation messages. The proxy detects these before forwarding and handles them locally.

Processing rules:
1. Control messages stripped from message list before delta computation
2. `message_count` tracks only non-control messages
3. Control messages are idempotent via hash tracking

### Scenario: Clean all sessions
- GIVEN 3 active sessions exist
- WHEN client sends message containing `control_clean_all`
- THEN all 3 sessions are cancelled and deleted

### Scenario: Extend session lifetime
- GIVEN current session expires at `1718570000`
- WHEN client sends message with `control_extend_lifetime` and timestamp `1719174800`
- THEN session's `expires_at_utc` updated to `1719174800`

### Scenario: Control message stripped from delta
- GIVEN 3 delivered messages, new request has 2 text + 1 control message
- WHEN computing delta
- THEN control excluded, delta = 2, `message_count` becomes 5

### Scenario: Control message idempotency
- GIVEN control message `clean_all` was processed
- WHEN client re-sends the same control message
- THEN it is ignored (hash matches already-processed set)

## Requirement: Control Constants HTTP Endpoint

```
GET /interactions/v1/control-constants
```

Returns JSON keyed by section name. Only sections with `endpoint_interactions` AND at least one control constant are included.

### Scenario: Constants returned
- GIVEN config has section with `endpoint_interactions`, `control_clean_all`, `control_extend_lifetime`
- WHEN endpoint is called
- THEN response maps section name to `{"clean_all": "...", "extend_lifetime": "..."}`

## Requirement: Egress Message Splitting (proxy_limit)

When `proxy_limit` is configured, the serialized `Content[]` is measured. If it exceeds the limit, the array is split into multiple interactions chained via `previous_interaction_id`.

- Sequential greedy packing — order preserved, no reordering
- First chunk uses session's existing `interaction_id` as `previous_interaction_id`
- Store the LAST chunk's `interaction.id` in session state
- `message_count` reflects total across all chunks

### Scenario: Split across multiple interactions
- GIVEN `proxy_limit = "15k"`, session has `interaction_id = "prior-id"`, 3 messages (6 + 6 + 4 KiB)
- WHEN request is processed
- THEN chunk1 (msg1+msg2, 12 KiB, `previous_interaction_id = "prior-id"`) → chunk2 (msg3, chained to chunk1)

### Scenario: Single element exceeds limit
- GIVEN `proxy_limit = "1k"`, message serializes to 5 KiB
- WHEN request is processed
- THEN 415 error returned, no interactions created

### Scenario: Hunks preserve message order (sequential greedy)
- GIVEN `proxy_limit = "10k"`, 3 messages (6 + 5 + 4 KiB)
- THEN chunk1 = [msg1] (6 KiB) → chunk2 = [msg2, msg3] (5+4=9 KiB). Order preserved.

## Requirement: Graceful Shutdown

`main.rs` handles SIGTERM/SIGINT via `tokio::signal`. On shutdown signal, the server stops accepting new connections and drains in-flight requests before exiting. This is the standard Axum graceful shutdown pattern.

## Requirement: SSE Response with Extra Header

`sse_response_with_extra_header()` in `sse.rs` extends the standard SSE response builder with one caller-specified header, used by the Interactions streaming path to include the session identifier.

### Scenario: SSE response carries session header
- GIVEN a streaming interactions response
- WHEN the SSE response is built
- THEN it includes the session header (`x-claude-code-session-id` or `x-request-id`) alongside `content-type: text/event-stream`

## Requirement: Control Action Helper

`handle_control_action(&self, action, session_id, route, ingress)` executes a control action and returns a 200 OK JSON response with the session identifier header. Used by both `handle_from_anthropic` and `handle_from_openai` to avoid duplicating the control message handling logic.

## Requirement: Fallback Response Builder

`build_fallback_response(last_interaction, last_id, model, ingress)` builds a protocol-appropriate response body (`ChatCompletionResponse` or `MessageResponse`) from interaction usage stats when `build_response_from_interaction` is unavailable. Used by `handle_split_send` and `send_split_system_instruction`.

## Requirement: OK Response with Session Header

`ok_with_session_header(ingress, session_id, json)` returns a `200 OK` response with the given JSON body and the session identifier header (`x-claude-code-session-id` for Anthropic ingress, `x-request-id` for OpenAI ingress). Replaces the repeated pattern of building a response and manually inserting the header.
