# Proposal: Translate Interactions Error Bodies

**Change ID:** `translate-interactions-error-bodies`
**Created:** 2026-06-21
**Status:** Draft

---

## Problem Statement

When the Gemini Interactions API returns an error (e.g., 429 quota exceeded), the response body is in Gemini format:

```json
{"error":{"message":"You do not have enough quota to make this request.","code":"too_many_requests"}}
```

This format differs from both Anthropic (`{"type":"error","error":{"type":"...","message":"..."}}`) and OpenAI (`{"error":{"message":"...","type":"...","code":"..."}}`) formats. 

The existing `apply_error_translation` mechanism only rewrites the body when a user-configured rule matches. Without such a rule, the Gemini error format passes through unchanged to the downstream agent. The agent cannot parse the unfamiliar format and retries the request indefinitely.

Gemini surfaces the quota error in two ways:
1. **Non-streaming:** HTTP 429 with Gemini JSON body — the format leaks through
2. **Streaming:** HTTP 200 with an `error` SSE event inside the stream — already correctly translated by `translate_stream_event`

This change addresses case 1.

## Proposed Solution

Add `translate_interactions_error_to_protocol` — a function that detects Gemini-shaped error bodies and reformats them to the ingress protocol format (Anthropic or OpenAI). Wire it into all 4 non-streaming error paths in `InteractionsHandler`.

For status 429 specifically, the function overrides the error code to `rate_limit_error` with a clear "do NOT retry" message — regardless of what Gemini chose for `error.code`.

The translation happens **before** `apply_error_translation`, so user-configured rules operate on the familiar Anthropic/OpenAI format.

## Scope

### In Scope
- Non-streaming Gemini error body → Anthropic/OpenAI format translation
- All 4 non-streaming error paths in `interactions_handler.rs`
- Config example `[[error_translation]]` for overriding specific status codes (e.g., 429 → `rate_limit_error`)

### Out of Scope
- Streaming SSE error events (already correctly translated at line ~1638)
- Changes to `apply_error_translation` itself
- Non-Gemini error bodies (pass through unchanged)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| Data layer | No | — |
| State | No | — |
| Config | No | — |
| Protocol conversion | Yes | New error body translation function + wiring |

## Success Criteria

- [ ] Gemini non-streaming error → client receives Anthropic/OpenAI-formatted error
- [ ] User-configured `[[error_translation]]` rules can override specific status codes (e.g., 429)
- [ ] Non-Gemini bodies pass through unchanged
- [ ] All existing tests pass, new tests cover translation logic

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Existing error_translation rules matching raw Gemini format break | Low | Low | Users would need to update rules to match Anthropic/OpenAI format; documented in release notes |
