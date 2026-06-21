# Delta: Routing

**Change ID:** `interactions-auth-header-tests`
**Affects:** `src/interactions_handler.rs` (test module), `tests/e2e.rs`

---

## ADDED

### Requirement: `build_interactions_headers` Unit Tests

The `build_interactions_headers` function must have direct unit tests covering all auth header combinations.

#### Scenario: x-goog-api-key set from api_key
- GIVEN `api_key = Some("gemini-key-123")`
- WHEN `build_interactions_headers` builds the upstream request
- THEN `x-goog-api-key: gemini-key-123` header is present

#### Scenario: Client auth headers stripped when api_key set
- GIVEN `api_key = Some(...)` and client sends `Authorization: Bearer sk-ant-...` and `x-api-key: old-key`
- WHEN `build_interactions_headers` builds the upstream request
- THEN `Authorization` and `x-api-key` headers are absent

#### Scenario: Client auth headers forwarded when no api_key
- GIVEN `api_key = None` and client sends `Authorization: Bearer sk-ant-...`
- WHEN `build_interactions_headers` builds the upstream request
- THEN `Authorization: Bearer sk-ant-...` header is forwarded

#### Scenario: Non-auth headers always forwarded
- GIVEN `api_key = Some(...)` and client sends `x-request-id: trace-123`
- WHEN `build_interactions_headers` builds the upstream request
- THEN `x-request-id: trace-123` is present regardless of api_key

### Requirement: Interactions Auth Header E2E Tests

Full dispatch-path E2E tests for auth header behavior through `InteractionsHandler`.

#### Scenario: E2E — client auth stripped with api_key
- GIVEN config has `api_key = "secret"` and client sends `Authorization: Bearer client-key`
- WHEN `POST /v1/messages` is dispatched to interactions upstream
- THEN upstream receives `x-goog-api-key: secret` and does NOT receive `Authorization`

#### Scenario: E2E — x-goog-api-key matches config
- GIVEN config has `api_key = "my-gemini-key"`
- WHEN `POST /v1/messages` is dispatched to interactions upstream
- THEN upstream receives `x-goog-api-key: my-gemini-key`
