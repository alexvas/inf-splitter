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

`GET /health` probes each unique upstream endpoint with a HEAD request:
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

## Requirement: Graceful Shutdown

`main.rs` handles SIGTERM/SIGINT via `tokio::signal`. On shutdown signal, the server stops accepting new connections and drains in-flight requests before exiting. This is the standard Axum graceful shutdown pattern.
