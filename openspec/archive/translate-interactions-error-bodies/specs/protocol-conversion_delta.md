# Delta: Protocol Conversion — Error Body Translation

**Change ID:** `translate-interactions-error-bodies`
**Affects:** `src/lib.rs`, `src/interactions_handler.rs`

---

## ADDED

### Requirement: Gemini Error Body Translation (Non-Streaming)

When the Interactions API returns a non-2xx status with a Gemini-shaped error body, the proxy translates it to the ingress protocol format before applying user-configured `error_translation` rules.

A Gemini error body has the shape `{"error": {"message": "...", "code": "..."}}` where `message` is a string. `code` defaults to `"api_error"` if absent. The function signature is `translate_interactions_error_to_protocol(body: &str, ingress: Protocol) -> String`.

#### Scenario: Gemini error → Anthropic format
- GIVEN interactions upstream returns body `{"error":{"message":"Quota exceeded","code":"too_many_requests"}}`
- WHEN `translate_interactions_error_to_protocol` is called with `Protocol::Anthropic`
- THEN the body is translated to `{"type":"error","error":{"type":"too_many_requests","message":"Quota exceeded"}}`

#### Scenario: Gemini error → OpenAI format
- GIVEN interactions upstream returns body `{"error":{"message":"Quota exceeded","code":"too_many_requests"}}`
- WHEN `translate_interactions_error_to_protocol` is called with `Protocol::OpenAi`
- THEN the body is translated to `{"error":{"message":"Quota exceeded","type":"too_many_requests","code":"too_many_requests"}}`

#### Scenario: Missing code defaults to api_error
- GIVEN interactions upstream returns body `{"error":{"message":"Internal error"}}`
- WHEN `translate_interactions_error_to_protocol` is called
- THEN `error.type` defaults to `"api_error"`

#### Scenario: Non-Gemini body passes through
- GIVEN interactions upstream returns plain text body `"upstream error"`
- WHEN `translate_interactions_error_to_protocol` is called
- THEN the body is returned unchanged

#### Scenario: Non-Gemini JSON passes through
- GIVEN interactions upstream returns body `{"error":{"type":"server_error"}}` (no `message` field)
- WHEN `translate_interactions_error_to_protocol` is called
- THEN the body is returned unchanged

#### Scenario: User rule overrides translated body
- GIVEN `translate_interactions_error_to_protocol` translates a Gemini error to Anthropic format
- AND a user-configured `[[error_translation]]` rule matches (e.g., `status = 429`)
- THEN `apply_error_translation` replaces the body with the rule's `egress`

---

## MODIFIED

### Requirement: Interactions Error Handling (Non-Streaming)

All 4 non-streaming error paths in `InteractionsHandler` now call `translate_interactions_error_to_protocol` before `apply_error_translation`:

- `send_and_translate`
- `handle_split_send`
- `send_split_system_instruction` first chunk
- `send_split_system_instruction` subsequent chunk

#### Scenario: All non-streaming error paths translate Gemini errors
- GIVEN any non-streaming interactions upstream error (non-2xx status)
- WHEN the error body is processed
- THEN `translate_interactions_error_to_protocol(&error_body, ingress)` is called before `apply_error_translation(sc, body, &self.error_translation)`
