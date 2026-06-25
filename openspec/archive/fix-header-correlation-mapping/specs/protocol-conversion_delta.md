# Delta: Protocol Conversion

**Change ID:** `fix-header-correlation-mapping`
**Affects:** `src/auth.rs`, `src/openai.rs`, `src/anthropic.rs`, `src/interactions_handler.rs`

---

## MODIFIED

### Requirement: Interactions Header Forwarding Maps Correlation IDs Correctly

**Change:** `build_interactions_headers_map` for Gemini no longer sets `X-Client-Request-Id`. Gemini is a stateful protocol — session continuity is via `previous_interaction_id` in request body. HTTP correlation headers are irrelevant.

Before (removed):
```
x-claude-code-session-id → X-Client-Request-Id
```

After:
Gemini receives neither `X-Client-Request-Id` nor `x-claude-code-session-id` mapping. Client headers pass through, but `x-request-id` is excluded (as before).

#### Scenario: Gemini does not receive X-Client-Request-Id
- GIVEN client sends `x-claude-code-session-id: sess-1`
- WHEN `build_interactions_headers_map` builds headers for Gemini
- THEN `X-Client-Request-Id` is NOT present
- AND `x-claude-code-session-id` is forwarded as-is (passthrough, for response header correlation)
