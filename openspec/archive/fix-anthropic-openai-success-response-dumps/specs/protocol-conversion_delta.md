# Delta: Protocol Conversion

**Change ID:** `fix-anthropic-openai-success-response-dumps`
**Affects:** Anthropic↔OpenAI conversion diagnostics

---

## ADDED

### Requirement: Conversion Handlers Preserve Raw Upstream Responses for Diagnostics

Conversion handlers must preserve raw upstream response bytes long enough to record diagnostics before translating the response back to the ingress protocol.

#### Scenario: Successful OpenAI JSON response is dumped before Anthropic translation
- GIVEN an Anthropic request is converted and sent to an OpenAI upstream
- AND the upstream returns a successful JSON chat completion response
- WHEN the handler translates the response back to Anthropic format
- THEN diagnostics record the raw OpenAI JSON response body before translation
- AND the client still receives the translated Anthropic response

#### Scenario: Successful Anthropic JSON response is dumped before OpenAI translation
- GIVEN an OpenAI request is converted and sent to an Anthropic upstream
- AND the upstream returns a successful JSON message response
- WHEN the handler translates the response back to OpenAI format
- THEN diagnostics record the raw Anthropic JSON response body before translation
- AND the client still receives the translated OpenAI response

#### Scenario: Successful OpenAI SSE response is dumped while Anthropic streaming translation continues
- GIVEN an Anthropic streaming request is converted and sent to an OpenAI upstream
- AND the upstream returns an OpenAI SSE stream
- WHEN the handler translates SSE chunks back to Anthropic SSE events
- THEN diagnostics capture the raw OpenAI SSE bytes without changing the client-visible translated stream

#### Scenario: Successful Anthropic SSE response is dumped while OpenAI streaming translation continues
- GIVEN an OpenAI streaming request is converted and sent to an Anthropic upstream
- AND the upstream returns an Anthropic SSE stream
- WHEN the handler translates SSE chunks back to OpenAI SSE events
- THEN diagnostics capture the raw Anthropic SSE bytes without changing the client-visible translated stream

---

## MODIFIED

(None)

---

## REMOVED

(None)
