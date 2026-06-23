# Proposal: Fix — Proxy for All Endpoints

**Change ID:** `fix-proxy-for-all-endpoints`
**Created:** 2026-06-23
**Status:** Implementation Complete
**Completed:** 2026-06-23

---

## Problem Statement

The `proxy` config parameter is defined per provider section and stored in `RouteTarget`, but it is **only applied in `InteractionsHandler`**. The `OpenAiHandler` and `AnthropicHandler` build their reqwest `Client` without ever consulting the route's proxy setting. This means operators who configure `proxy` for OpenAI or Anthropic upstreams get no effect — the requests go out directly, bypassing the configured proxy.

This is a bug, not a missing feature: the config field is parsed, stored, documented as "per-endpoint proxy", and tested — but silently ignored for 2 of the 3 protocol handlers.

## Proposed Solution

Apply `route.proxy` when building outgoing HTTP requests in `OpenAiHandler` and `AnthropicHandler`.

Since the handlers are constructed once (not per-request) but the proxy URL comes from the route (per-request), each handler will maintain an internal `HashMap<String, HttpClient>` cache keyed by proxy URL. On each request, the handler:
1. Looks up `route.proxy` → if `None`, uses the default client (no proxy, current behavior)
2. If `Some(url)`, checks the cache for a client with that proxy, building one if missing
3. Uses the resulting client for the upstream call

A special key `""` (empty string) represents the no-proxy default client to avoid unnecessary branching in request-building code.

## Scope

### In Scope
- `OpenAiHandler`: use `route.proxy` for all upstream requests (passthrough openai→openai, conversion anthropic→openai)
- `AnthropicHandler`: use `route.proxy` for all upstream requests (passthrough anthropic→anthropic, conversion openai→anthropic)
- Update spec `config.md` to clarify proxy applies to all endpoint types

### Out of Scope
- Changing proxy behavior for `InteractionsHandler` (already works)
- Per-protocol proxy (separate proxy for OpenAI vs Anthropic within same section — not needed)
- SOCKS proxy support changes (already supported by reqwest)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/openai.rs` | Yes | Replace single `http: HttpClient` with `clients: HashMap<String, HttpClient>`, resolve per-request |
| `src/anthropic.rs` | Yes | Same as openai.rs |
| `src/interactions_handler.rs` | No | Already applies proxy correctly |
| `src/config.rs` | No | `proxy` field already present on `RouteTarget` |
| Spec `config.md` | Yes | Clarify proxy scope |

## Architecture Considerations

The client-per-proxy cache pattern already exists conceptually in `InteractionsHandler` (which builds one client with the first interactions section's proxy). The difference: `InteractionsHandler` picks the proxy at construction time (one shot), while `OpenAiHandler`/`AnthropicHandler` must resolve it per-request because routes can have different proxies.

A `HashMap<String, Client>` with ~3 entries is the practical case (most deployments have one proxy or none). The `Client` type is `Clone` but cloning still shares the connection pool — building a new `Client` per unique proxy URL is correct and performant.

## Success Criteria

- [x] `proxy` in a config section with `endpoint_openai` causes outgoing requests to go through that proxy
- [x] `proxy` in a config section with `endpoint_anthropic` causes outgoing requests to go through that proxy
- [x] Sections without `proxy` continue to use no proxy (env vars only)
- [x] Spec updated to reflect proxy works for all endpoint types
- [x] All existing tests pass
- [x] Shared `build_http_client` / `build_client_map` helpers extracted to `lib.rs` (all three handlers)

---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/lib.rs` — added `build_http_client()` and `build_client_map()` shared helpers
- `src/openai.rs` — `http: HttpClient` → `clients: HashMap<String, HttpClient>`, `get_client()` per-request resolution
- `src/anthropic.rs` — `client: Client` → `clients: HashMap<String, Client>`, `get_client()` per-request resolution
- `src/interactions_handler.rs` — `http: HttpClient` → `clients: HashMap<String, HttpClient>`, `get_client()` per-request resolution (was single-client, now consistent with other handlers)
- `openspec/specs/config.md` — "Per-Endpoint Proxy" → "Per-Section Proxy", expanded scenarios

### Specs Updated
- `openspec/specs/config.md` — proxy requirement now covers all three endpoint types
