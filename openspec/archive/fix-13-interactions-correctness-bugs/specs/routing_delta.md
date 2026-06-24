# Delta: Routing

**Change ID:** `fix-13-interactions-correctness-bugs`
**Affects:** `src/router.rs`, `src/auth.rs`

---

## ADDED

### Requirement: Control Constants Endpoint Intentionally Unprotected

`GET /interactions/v1/control-constants` is intentionally left without authentication. Proxy access is controlled at the environment level (local process or container). The endpoint exposes sentinel strings used for in-band control commands (`clean_all`, `extend_lifetime`).

The sentinels themselves are protected against accidental triggering via a deduplication requirement: the sentinel text must appear twice consecutively in the message to activate. See protocol-conversion delta for details.

#### Scenario: Endpoint returns constants without auth
- GIVEN any request to `GET /interactions/v1/control-constants`
- WHEN no auth header is present
- THEN 200 with JSON constants is returned (no authentication required)

### Requirement: API Key Validation at Config Load

`api_key` values must be validated as legal HTTP header values at config load time. Keys containing bytes outside the visible ASCII range, newlines, or other characters illegal in HTTP headers are rejected with a clear error message.

#### Scenario: Valid API key accepted
- GIVEN `api_key = "${VALID_KEY}"` where secret is `sk-abc123`
- WHEN config is loaded
- THEN the key is accepted and used as `x-goog-api-key` / `Authorization` header value

#### Scenario: Invalid API key rejected
- GIVEN `api_key = "${BAD_KEY}"` where secret contains newline characters
- WHEN config is loaded
- THEN startup fails with `ConfigError::InvalidApiKey { section, message: "api_key contains invalid HTTP header bytes" }`

#### Scenario: Empty API key rejected
- GIVEN `api_key = "${EMPTY_KEY}"` where secret is empty string
- WHEN config is loaded
- THEN startup fails with `ConfigError::InvalidApiKey { section, message: "api_key must not be empty" }`

## MODIFIED

(None)

## REMOVED

(None)
