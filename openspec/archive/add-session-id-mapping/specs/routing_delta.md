# Delta: Routing — Session ID Mapping

**Change ID:** `add-session-id-mapping`
**Affects:** `src/auth.rs`, `src/interactions_handler.rs`, `src/sse.rs`

---

## MODIFIED

### Requirement: Session ID Resolution

`resolve_session_id()` in `interactions_handler.rs` resolves the session identifier for Interactions requests.

**Priority order:**
1. HTTP header `x-request-id` (primary)
2. HTTP header `x-claude-code-session-id` (Claude CLI)
3. Body field `request_id` (fallback)
4. Random UUID v7 (last resort)

#### Scenario: Claude CLI session ID
- GIVEN `x-claude-code-session-id: 1b9db61a-154f-45ba-827c-6f898f4cf831` header and no `x-request-id`
- WHEN session is resolved
- THEN `session_id = "1b9db61a-154f-45ba-827c-6f898f4cf831"`

#### Scenario: x-request-id still wins
- GIVEN both `x-request-id: req-123` and `x-claude-code-session-id: session-456`
- WHEN session is resolved
- THEN `session_id = "req-123"` (x-request-id wins)

---

### Requirement: Session Identifier Header Mapping (Egress)

`forward_request_headers_map()` in `auth.rs` adds complementary session identifier headers when forwarding to upstreams. If one header is present and the other is absent, the missing header is inserted with the same value.

#### Scenario: Anthropic client → adds x-request-id for OpenAI upstream
- GIVEN client sends `x-claude-code-session-id: abc123` but no `x-request-id`
- WHEN headers are forwarded
- THEN both `x-claude-code-session-id: abc123` and `x-request-id: abc123` are present

#### Scenario: OpenAI client → adds x-claude-code-session-id for Anthropic upstream
- GIVEN client sends `x-request-id: def456` but no `x-claude-code-session-id`
- WHEN headers are forwarded
- THEN both `x-request-id: def456` and `x-claude-code-session-id: def456` are present

#### Scenario: Both headers present — no overwrite
- GIVEN client sends both `x-request-id: req-789` and `x-claude-code-session-id: sess-789`
- WHEN headers are forwarded
- THEN each keeps its original value unchanged

---

### Requirement: Interactions Session Response Header

All `InteractionsHandler` response paths return the resolved session ID as a response header. The header name depends on ingress protocol:
- Anthropic ingress → `x-claude-code-session-id: <session_id>`
- OpenAI ingress → `x-request-id: <session_id>`

The header is set via `session_header_name(ingress)` which maps `Protocol::Anthropic` → `"x-claude-code-session-id"`, `Protocol::OpenAi` → `"x-request-id"`.

Covered response paths:
- `send_and_translate` — non-streaming success and error
- `handle_stream_response` — streaming (via `sse_response_with_extra_header`)
- `handle_split_send` — split-send chunk responses
- Control message responses (clean-all, extend-lifetime)

#### Scenario: Anthropic client gets x-claude-code-session-id
- GIVEN an Anthropic ingress request with `x-claude-code-session-id: abc`
- WHEN the Interactions response is returned
- THEN response includes header `x-claude-code-session-id: abc`

#### Scenario: OpenAI client gets x-request-id
- GIVEN an OpenAI ingress request with `x-request-id: def`
- WHEN the Interactions response is returned
- THEN response includes header `x-request-id: def`

---

## ADDED

### Requirement: SSE Response with Extra Header

`sse_response_with_extra_header()` in `sse.rs` extends the standard SSE response builder with one caller-specified header, used by the Interactions streaming path to include the session identifier.

#### Scenario: SSE response carries session header
- GIVEN a streaming interactions response
- WHEN the SSE response is built
- THEN it includes the session header (`x-claude-code-session-id` or `x-request-id`) alongside `content-type: text/event-stream`
