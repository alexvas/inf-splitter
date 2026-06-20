# Proposal: Fix Interactions Response Translation

**Change ID:** `fix-interactions-response-translation`
**Created:** 2026-06-19
**Status:** Implementation Complete
**Completed:** 2026-06-20

---

## Problem Statement

The delta spec for `add-interactions-protocol` defines scenarios for **request** translation
direction only:

> WHEN `POST /v1/messages` arrives → THEN the request is translated Anthropic→Interactions
> WHEN `POST /v1/chat/completions` arrives → THEN the request is translated OpenAI→Interactions

It never explicitly states that the **response** must be translated back to the **client's
ingress protocol**. Because of this missing requirement, the implementation always returns
Anthropic-format responses (both in `send_and_translate` and `handle_stream_response`).

An OpenAI client sending `POST /v1/chat/completions` receives an Anthropic-shaped JSON
response (or SSE events) instead of OpenAI format. This breaks the API contract — clients
expect responses in the same protocol they sent.

## Proposed Solution

### Fix the spec

Add explicit response-translation scenarios to `routing_delta.md`:

- Anthropic ingress → Interactions → **Anthropic response**
- OpenAI ingress → Interactions → **OpenAI response**

For both non-streaming and streaming paths.

### Fix the implementation

**Non-streaming path** (`send_and_translate`):
- After receiving the `Interaction` response from upstream, branch on ingress protocol:
  - Anthropic ingress → build Anthropic `MessageResponse` (existing code)
  - OpenAI ingress → build OpenAI `ChatCompletionResponse` using `anyllm_translate::translate_response`

**Streaming path** (`handle_stream_response`):
- `StreamEvent` objects are always Anthropic format. For OpenAI clients, pipe them through
  `StreamingTranslator` from `anyllm_translate::mapping::streaming_map` (already used in
  `OpenAiHandler::handle_stream_manual`) to produce `ChatCompletionChunk` SSE output.

## Scope

### In Scope
- Response translation direction: Interactions → OpenAI (non-streaming)
- Response translation direction: Interactions → OpenAI (streaming SSE)
- Spec update: add response-translation scenarios
- Unit tests: `translate_stream_event` → OpenAI chunk translation
- E2E test: OpenAI ingress → Interactions → OpenAI streaming response

### Out of Scope
- Changing the request translation path (already correct)
- OpenAPI schema changes

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `routing_delta.md` | Yes | Add response-translation scenarios |
| `interactions_handler.rs` | Yes | Branch on ingress protocol in `send_and_translate` and `handle_stream_response` |
| `tests/e2e.rs` | Yes | E2E test for OpenAI→Interactions→OpenAI streaming |

## Success Criteria

- [ ] Spec updated with explicit response-translation scenarios
- [ ] Non-streaming OpenAI→Interactions returns OpenAI-formatted JSON
- [ ] Streaming OpenAI→Interactions returns OpenAI SSE chunks
- [ ] All existing tests pass (no regression for Anthropic path)
- [ ] RED-GREEN: new tests verify both directions

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Breaking Anthropic response path | Low | High | Existing E2E tests cover Anthropic path |
| StreamingTranslator API mismatch | Low | Medium | Already used in `OpenAiHandler` — same pattern |

---

## Archive Information

**Archived:** 2026-06-20
**Duration:** 1 day (2026-06-19 → 2026-06-20)
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — `send_and_translate` and `handle_stream_response` branch on ingress `Protocol` to return Anthropic or OpenAI format; typed response structs replace `json!()`
- `CLAUDE.md` — added rule: prefer strict type-checked structs over string snippets

### Specs Updated
- `openspec/specs/routing.md` — added "Interactions Dispatch" and "Response Translation to Client Protocol" requirements with 6 scenarios
