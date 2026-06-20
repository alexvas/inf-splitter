# Delta: Response Translation (Interactions → Client Protocol)

**Change ID:** `fix-interactions-response-translation`
**Affects:** `src/interactions_handler.rs`, `openspec/changes/add-interactions-protocol/specs/routing_delta.md`

---

## ADDED

### Requirement: Response Translation to Client Protocol

When `InteractionsHandler` receives an upstream response, it MUST translate the
response back to the client's ingress protocol format.

| Ingress | Upstream | Non-streaming response | Streaming response |
|---------|----------|----------------------|--------------------|
| Anthropic | Interactions | Anthropic `MessageResponse` JSON | Anthropic `StreamEvent` SSE |
| OpenAI | Interactions | OpenAI `ChatCompletionResponse` JSON | OpenAI `ChatCompletionChunk` SSE |

#### Scenario: Anthropic ingress → Interactions → Anthropic response (non-streaming)
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/messages` arrives without `stream: true`
- THEN the request is translated Anthropic→Interactions upstream
- AND the upstream `Interaction` is translated to an Anthropic `MessageResponse` JSON
- AND the client receives `{"type":"message","role":"assistant","content":[...],...}`

#### Scenario: OpenAI ingress → Interactions → OpenAI response (non-streaming)
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/chat/completions` arrives without `stream: true`
- THEN the request is translated OpenAI→Interactions upstream
- AND the upstream `Interaction` is translated to an OpenAI `ChatCompletionResponse` JSON
- AND the client receives `{"object":"chat.completion","choices":[...],...}`

#### Scenario: Anthropic ingress → Interactions → Anthropic SSE (streaming)
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/messages` arrives with `stream: true`
- THEN the request is translated Anthropic→Interactions upstream with `stream: true`
- AND the upstream SSE events are parsed and translated to Anthropic `StreamEvent` SSE
- AND the client receives `event: message_start\ndata: {...}\n\nevent: content_block_delta\n...`

#### Scenario: OpenAI ingress → Interactions → OpenAI SSE (streaming)
- GIVEN section has only `endpoint_interactions`
- WHEN `POST /v1/chat/completions` arrives with `stream: true`
- THEN the request is translated OpenAI→Interactions upstream with `stream: true`
- AND the upstream SSE events are parsed, translated to Anthropic `StreamEvent`, then
  converted to OpenAI `ChatCompletionChunk` SSE via `StreamingTranslator`
- AND the client receives `data: {"choices":[{"delta":{"content":"..."},...}],...}\n\ndata: [DONE]\n\n`

## MODIFIED

### Requirement: Interactions Dispatch (routing_delta.md)

The dispatch table is updated to include response format:

| Ingress | Interactions endpoint set | Action |
|---------|--------------------------|--------|
| OpenAI | Yes | `InteractionsHandler::handle_from_openai()` → returns OpenAI format |
| Anthropic | Yes | `InteractionsHandler::handle_from_anthropic()` → returns Anthropic format |