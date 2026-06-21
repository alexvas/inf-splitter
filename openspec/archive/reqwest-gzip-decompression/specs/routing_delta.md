# Delta: Routing

**Change ID:** `reqwest-gzip-decompression`
**Affects:** `src/interactions_handler.rs` (`build_interactions_headers`)

---

## MODIFIED

### Requirement: Interactions Dispatch

When a request is routed to `InteractionsHandler` and the section has a configured `api_key`:
- Client auth headers (`Authorization`, `x-api-key`) are **not forwarded** to the interactions upstream
- Only `x-goog-api-key` is sent for authentication
- Non-auth client headers (e.g., `x-request-id`) are forwarded as usual

When `api_key` is not configured, no auth headers are added and client auth headers (if any) pass through unchanged.

#### Scenario: Client auth headers suppressed
- GIVEN section has `api_key = "${GEMINI_API_KEY}"` and client sends `Authorization: Bearer sk-ant-...`
- WHEN `build_interactions_headers` builds the upstream request
- THEN `Authorization` header is stripped, only `x-goog-api-key` is sent

#### Scenario: No API key configured
- GIVEN section has no `api_key`
- WHEN `build_interactions_headers` builds the upstream request
- THEN client auth headers (if any) are forwarded unchanged
