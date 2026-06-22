# Proposal: Session ID Mapping Across Protocol Boundaries

**Change ID:** `add-session-id-mapping`
**Created:** 2026-06-22
**Status:** Implemented

---

## Problem Statement

Claude CLI sends `x-claude-code-session-id` header (NOT `x-request-id`). The proxy:

- Only recognizes `x-request-id` for Interactions session identification — Claude CLI sessions get random UUIDs, breaking session affinity
- Does not map `x-claude-code-session-id` ↔ `x-request-id` when forwarding between Anthropic and OpenAI protocols
- Does not return the session identifier as a response header for Interactions clients

Each protocol has its own session identifier convention:

| Protocol | Identifier | Location |
|----------|-----------|----------|
| Anthropic (Claude CLI) | `x-claude-code-session-id` | Request header |
| OpenAI | `x-request-id` | Request/response header |
| Interactions (Gemini) | Proxy-internal `session_id` | Resolved from headers, returned as response header |

Without translation, Claude CLI sessions fragment into random UUIDs on each Interactions request, breaking delta computation and session continuity.

## Proposed Solution

1. **Session ID resolution**: Add `x-claude-code-session-id` to the priority chain in `resolve_session_id()` — right after `x-request-id`, before body field
2. **Egress header mapping**: In `forward_request_headers_map()`, add symmetric mapping — if one header is present and the other absent, insert the missing one. Covers both Anthropic→OpenAI and OpenAI→Anthropic directions
3. **Response header mapping**: In `relay_response_headers()` (OpenAI upstream → client) and `copy_response_headers()` (Anthropic upstream → client), add the complementary header mapped from the known one
4. **Interactions response headers**: In all `InteractionsHandler` response paths (sync, stream, split-send, control messages, errors), return `x-claude-code-session-id` (Anthropic ingress) or `x-request-id` (OpenAI ingress) as response header

## Scope

### In Scope
- Session ID resolution: `x-claude-code-session-id` recognized in Interactions handler
- Egress: bidirectional `x-claude-code-session-id` ↔ `x-request-id` mapping in `forward_request_headers_map`
- Response: relay and mapping in `copy_response_headers` (anthropic.rs) and `relay_response_headers` (openai.rs)
- Interactions: session ID returned as response header in all paths (sync, stream, split-send, control, error)
- SSE utility: `sse_response_with_extra_header` for streaming responses

### Out of Scope
- Compression (brotli/zstd/deflate) — already implemented and archived as `enable-http-compression`
- `sessionId` field in Interactions API request body — not part of the Gemini Interactions protocol; proxy uses `session_id` internally only

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/auth.rs` | Yes | `forward_request_headers_map` — bidirectional mapping |
| `src/openai.rs` | Yes | `relay_response_headers` — add `x-claude-code-session-id`, map from `x-request-id` |
| `src/anthropic.rs` | Yes | `copy_response_headers` — add `x-claude-code-session-id`, `x-request-id`, map `x-claude-code-session-id → x-request-id` |
| `src/interactions_handler.rs` | Yes | `resolve_session_id`, `session_header_name`, response headers in all paths |
| `src/sse.rs` | Yes | `sse_response_with_extra_header` |
| `Cargo.toml` | No | — |

## Architecture Considerations

The mapping is symmetric: if header A is present and header B is absent, insert B from A's value. This avoids overwriting when both are present (unlikely but possible). The pattern is applied identically across all three mapping sites (egress, OpenAI→client response, Anthropic→client response).

For Interactions, there is no upstream `sessionId` field — the session ID is purely proxy-internal. It is returned to the client as a response header so the client can use it in subsequent requests. The header name depends on ingress protocol: `x-claude-code-session-id` for Anthropic, `x-request-id` for OpenAI.

## Success Criteria

- [x] `resolve_session_id` recognizes `x-claude-code-session-id` (after `x-request-id`, before body)
- [x] Egress: `forward_request_headers_map` maps bidirectionally
- [x] Response: `relay_response_headers` maps `x-request-id → x-claude-code-session-id`
- [x] Response: `copy_response_headers` maps `x-claude-code-session-id → x-request-id`
- [x] Interactions: all response paths return session header
- [x] `cargo test` — 247 unit + 63 integration = 0 failures
- [x] `cargo clippy --locked -- -D warnings` — clean
- [x] `cargo fmt --check` — clean

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Client sends both headers with different values | Low | Low | Neither is overwritten; both forwarded as-is |
| Interactions API adds `sessionId` field in future | Low | Low | Proxy's internal session_id can be piped into it if needed |

---

## Archive Information

**Archived:** 2026-06-22
**Duration:** < 1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/auth.rs` — `forward_request_headers_map` bidirectional mapping
- `src/openai.rs` — `relay_response_headers` whitelist + mapping
- `src/anthropic.rs` — `copy_response_headers` whitelist + mapping
- `src/interactions_handler.rs` — `resolve_session_id`, `session_header_name`, response headers
- `src/sse.rs` — `sse_response_with_extra_header`

### Specs Updated
- `openspec/specs/routing.md` — Session ID resolution, egress mapping, response headers, SSE utility
- `openspec/specs/protocol-conversion.md` — Anthropic + OpenAI response header whitelists
