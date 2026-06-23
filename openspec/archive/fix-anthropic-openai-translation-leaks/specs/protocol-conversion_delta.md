# Delta: Protocol Conversion

**Change ID:** `fix-anthropic-openai-translation-leaks`
**Affects:** Anthropic→OpenAI translation path (`src/openai.rs`)

---

## ADDED

### Requirement: Anthropic `extra` Field Sanitization

After Anthropic→OpenAI translation (`MessageCreateRequest` → `ChatCompletionRequest`), known Anthropic-specific fields that leak through `req.extra.clone()` are stripped from the outgoing request:

- `context_management` — Claude Code context management extension
- `output_config` — Claude Code output configuration (e.g., `effort`)

This happens in `sanitize_openai_egress()`, called before serialization (after `cap_openai_max_tokens`).

#### Scenario: context_management stripped from OpenAI egress
- GIVEN an Anthropic ingress body with `"context_management": {"edits": [...]}`
- WHEN `sanitize_openai_egress` processes the translated `ChatCompletionRequest`
- THEN `extra.remove("context_management")` is called
- AND the outgoing JSON body does not contain `context_management`

#### Scenario: output_config stripped from OpenAI egress
- GIVEN an Anthropic ingress body with `"output_config": {"effort": "max"}`
- WHEN `sanitize_openai_egress` processes the translated `ChatCompletionRequest`
- THEN `extra.remove("output_config")` is called
- AND the outgoing JSON body does not contain `output_config`

---

## MODIFIED

### Requirement: Anthropic→OpenAI Translation

**Updated:** After translation and token capping, `sanitize_openai_egress()` cleans the request:

- `max_tokens` is set to `None` when `max_completion_tokens` is present (always true for Anthropic→OpenAI translation). Newer OpenAI models (gpt-5.*, o-series) reject `max_tokens` and require `max_completion_tokens`.
- Known Anthropic `extra` fields (`context_management`, `output_config`) are removed.

When `route.max_tokens` is configured but `route.max_completion_tokens` is not, the limit is transferred to `max_completion_tokens` before `max_tokens` is removed, preserving route-level token caps.

Both streaming (`handle_stream_manual`) and non-streaming (`handle_sync_manual`) paths are updated.

### Requirement: Token Limit Injection

**Updated:** For Anthropic→OpenAI translation, `max_tokens` is removed from the outgoing request in favor of `max_completion_tokens`. If `route.max_tokens` is set but `route.max_completion_tokens` is not, the route-level limit is transferred to `max_completion_tokens` before `max_tokens` is removed.

#### Scenario: max_tokens removed for modern models
- GIVEN Anthropic→OpenAI translation produces `max_tokens: 32000, max_completion_tokens: 32000`
- WHEN `sanitize_openai_egress` is called
- THEN `max_tokens` is set to `None`
- AND `max_completion_tokens` is preserved as `Some(32000)`

#### Scenario: route.max_tokens limit preserved
- GIVEN `route.max_tokens = Some(1024)` and `route.max_completion_tokens = None`
- AND translation produces `max_tokens: 32000, max_completion_tokens: 32000`
- AFTER `cap_openai_max_tokens`: `max_tokens = Some(1024)`, `max_completion_tokens = Some(32000)`
- WHEN the limit transfer logic runs before `sanitize_openai_egress`
- THEN `max_completion_tokens` is clamped to `Some(1024)` (respecting `route.max_tokens`)
- AND `sanitize_openai_egress` sets `max_tokens = None`
- AND the outgoing request has `max_completion_tokens: 1024`, no `max_tokens`
