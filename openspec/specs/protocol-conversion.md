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
