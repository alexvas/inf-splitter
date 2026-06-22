# Delta: Routing — Interactions Handler Internals

**Change ID:** `deduplicate-interactions-handler`
**Affects:** `src/interactions_handler.rs`

---

## ADDED

### Requirement: Control Action Helper

`handle_control_action(&self, action, session_id, route, ingress)` executes a control action and returns a 200 OK JSON response with the session identifier header. Used by both `handle_from_anthropic` and `handle_from_openai`.

### Requirement: Fallback Response Builder

`build_fallback_response(last_interaction, last_id, model, ingress)` builds a protocol-appropriate response body (`ChatCompletionResponse` or `MessageResponse`) from interaction usage stats when `build_response_from_interaction` is unavailable.

### Requirement: OK Response with Session Header

`ok_with_session_header(ingress, session_id, json)` returns a `200 OK` response with the given JSON body and the session identifier header (`x-claude-code-session-id` for Anthropic ingress, `x-request-id` for OpenAI ingress).

---

## REMOVED

(None)
