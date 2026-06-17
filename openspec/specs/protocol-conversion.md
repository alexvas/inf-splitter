# Spec: Protocol Conversion

Components: `src/openai.rs`, `src/anthropic.rs`, `src/sse.rs`, `src/relay.rs`

## Requirement: OpenAI→Anthropic Translation

When OpenAI-format ingress must reach an Anthropic-compatible upstream, `AnthropicHandler` converts:
- `ChatCompletionRequest` → `MessageCreateRequest` via `anyllm_translate`
- Request body is parsed, translated, serialized, and sent upstream
- Response is translated back from Anthropic to OpenAI format

### Scenario: Non-streaming conversion
- GIVEN `POST /v1/chat/completions` with `"stream": false`
- WHEN routing to an Anthropic-only section
- THEN the full Anthropic response is collected and translated back to OpenAI chat completion format

### Scenario: Streaming conversion
- GIVEN `POST /v1/chat/completions` with `"stream": true`
- WHEN routing to an Anthropic-only section
- THEN each SSE event from the upstream is translated to OpenAI streaming format and streamed back

## Requirement: Anthropic→OpenAI Translation

When Anthropic-format ingress must reach an OpenAI-compatible upstream, `OpenAiHandler` converts:
- `MessageCreateRequest` → `ChatCompletionRequest` via `anyllm_translate`
- Request body is parsed, translated, serialized, and sent upstream
- Response is translated back from OpenAI to Anthropic format

Before translation, `strip_adaptive_thinking()` removes the `thinking` field when its type is `"adaptive"` — unsupported by `anyllm_translate` 0.9.x `ThinkingConfig`. Since the translation doesn't propagate thinking blocks anyway, removal is safe.

### Scenario: Non-streaming conversion
- GIVEN `POST /v1/messages` with `"stream": false`
- WHEN routing to an OpenAI-only section
- THEN the full OpenAI response is collected and translated back to Anthropic message format

### Scenario: Streaming conversion
- GIVEN `POST /v1/messages` with `"stream": true`
- WHEN routing to an OpenAI-only section
- THEN each SSE event from the upstream is translated to Anthropic streaming format and streamed back

## Requirement: Token Limit Injection

Token limits (`max_tokens`, `max_output_tokens`, `max_completion_tokens`) are injected into outgoing requests:
- **Passthrough paths:** JSON body is parsed, `cap_numeric_field()` clamps or sets the field
- **Conversion paths:** typed request structs are mutated before serialization
- `max_output_tokens` only applies via JSON passthrough (Anthropic struct has only `max_tokens`)

### Scenario: Passthrough token cap
- GIVEN config has `max_tokens = 100` and client sends `max_tokens: 500`
- WHEN request is sent upstream via passthrough
- THEN the outgoing body has `max_tokens: 100`

### Scenario: Conversion token cap
- GIVEN config has `max_tokens = 100` and client sends `max_tokens: 500`
- WHEN request is translated and sent upstream
- THEN the outgoing `MessageCreateRequest.max_tokens` is `100`

## Requirement: stream_options Handling

`stream_options` is always dropped from OpenAI requests before forwarding upstream. This is hardcoded.

### Scenario: stream_options stripped
- GIVEN a request body includes `"stream_options": {...}`
- WHEN the body is forwarded to an OpenAI upstream
- THEN `stream_options` is removed from the outgoing body

## Requirement: SSE Utilities

`src/sse.rs` provides shared utilities for detecting and handling Server-Sent Events:
- Detection of `text/event-stream` content type
- SSE line parsing
- Event formatting for downstream streaming responses

### Scenario: SSE content type detection
- GIVEN an upstream response with `Content-Type: text/event-stream`
- WHEN the handler receives the response
- THEN it enters streaming mode, parsing SSE events line by line

## Requirement: Diagnostic Relay Stream

`DiagnosticStream` wraps upstream SSE streams to buffer response data for diagnostics dumps:
- Buffers up to `MAX_STREAMING_DUMP_BYTES`
- On stream termination, records the buffered body as a dump event
- Drops buffering once a dump has been recorded for the stream

### Scenario: Streaming response dump
- GIVEN `dump_mode = "all"` and a streaming request
- WHEN the SSE stream completes
- THEN the accumulated body is recorded as an egress response dump

## Requirement: Drop Fields on Egress

`drop_fields` are applied to outgoing request bodies on all four routing paths. The shared helper `apply_egress_transforms` handles passthrough paths (token caps + field drops on parsed `Value`). The helper `prepare_egress_body` handles conversion paths (serialize struct → Value → drop → bytes).

### Scenario: Drop on passthrough
- GIVEN `drop_fields = ["user"]` on a passthrough section
- WHEN a request with `"user":"abc"` is processed
- THEN the upstream receives the body without the `user` field

### Scenario: Drop on conversion
- GIVEN `drop_fields = ["logprobs"]` and an OpenAI→Anthropic conversion section
- WHEN an OpenAI ingress request includes `"logprobs": true`
- THEN the translated Anthropic request does not contain `logprobs`

## Requirement: apply_egress_transforms

`lib.rs` provides `apply_egress_transforms(value, model, route)` for passthrough paths — applies token caps via `apply_token_caps_to_value`, resolves `drop_fields` for the model, and removes them via `drop_fields_from_value`. Used by both `OpenAiHandler` and `AnthropicHandler` passthrough.

## Requirement: prepare_egress_body

`lib.rs` provides `prepare_egress_body(req, model, route, diagnostics)` for conversion paths — serializes a typed request struct to `serde_json::Value`, applies `drop_fields`, serializes to bytes, and returns `PreparedBody { bytes, value, egress_str }`. Used by all four conversion paths (both streaming and non-streaming in both files).

## Requirement: Anthropic Response Header Whitelist

`copy_response_headers()` in `anthropic.rs` forwards only these response headers from upstream to client:
- `content-type`
- `request-id`
- `anthropic-ratelimit-requests-limit`
- `anthropic-ratelimit-requests-remaining`
- `anthropic-ratelimit-requests-reset`
- `anthropic-ratelimit-tokens-limit`
- `anthropic-ratelimit-tokens-remaining`
- `anthropic-ratelimit-tokens-reset`

All other upstream response headers are filtered out. The OpenAI response relay path applies its own header forwarding via `relay_response_headers`.
