# Delta: Protocol Conversion — Response Header Mapping

**Change ID:** `add-session-id-mapping`
**Affects:** `src/openai.rs`, `src/anthropic.rs`

---

## MODIFIED

### Requirement: Anthropic Response Header Whitelist

`copy_response_headers()` in `anthropic.rs` forwards these response headers from upstream to client:

- `content-type`
- `request-id`
- `x-request-id`
- `x-claude-code-session-id`
- `anthropic-ratelimit-requests-limit`
- `anthropic-ratelimit-requests-remaining`
- `anthropic-ratelimit-requests-reset`
- `anthropic-ratelimit-tokens-limit`
- `anthropic-ratelimit-tokens-remaining`
- `anthropic-ratelimit-tokens-reset`

Additionally, if `x-claude-code-session-id` is present and `x-request-id` is absent, `x-request-id` is inserted with the same value — so OpenAI clients get their expected header when the upstream is Anthropic.

#### Scenario: Anthropic upstream → OpenAI client gets x-request-id
- GIVEN Anthropic upstream response has `x-claude-code-session-id: sess-1`
- WHEN `copy_response_headers` processes the response
- THEN both `x-claude-code-session-id: sess-1` and `x-request-id: sess-1` are relayed

---

### Requirement: OpenAI Response Header Whitelist

`relay_response_headers()` in `openai.rs` forwards these response headers from upstream to client:

- `content-type`
- `x-ratelimit-*` (all rate-limit headers)
- `x-request-id`
- `request-id`
- `x-claude-code-session-id`
- `openai-*` (all OpenAI-specific headers)

Additionally, if `x-request-id` (or `request-id`) is present and `x-claude-code-session-id` is absent, `x-claude-code-session-id` is inserted with the same value — so Anthropic (Claude CLI) clients get their expected header when the upstream is OpenAI.

#### Scenario: OpenAI upstream → Anthropic client gets x-claude-code-session-id
- GIVEN OpenAI upstream response has `x-request-id: req-abc`
- WHEN `relay_response_headers` processes the response
- THEN both `x-request-id: req-abc` and `x-claude-code-session-id: req-abc` are relayed
