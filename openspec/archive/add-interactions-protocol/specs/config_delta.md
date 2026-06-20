# Delta: Configuration

**Change ID:** `add-interactions-protocol`
**Affects:** `src/config.rs`

---

## ADDED

### Requirement: Interactions Endpoint

Provider sections can specify `endpoint_interactions` — a Gemini Interactions API endpoint. This is valid alongside or instead of `endpoint_openai` / `endpoint_anthropic`. At least one endpoint must be set.

```toml
[gemini]
endpoint_interactions = "https://generativelanguage.googleapis.com/v1beta/interactions"
api_key = "${GEMINI_API_KEY}"
models = "gemini-3.1-flash-lite"
```

#### Scenario: Interactions endpoint only
- GIVEN section has only `endpoint_interactions`
- WHEN a request arrives for a matching model
- THEN the proxy translates to interactions format and calls the endpoint

#### Scenario: Interactions + Anthropic endpoints
- GIVEN section has `endpoint_interactions` and `endpoint_anthropic`
- WHEN Anthropic ingress arrives → passthrough to `endpoint_anthropic`
- WHEN OpenAI ingress arrives → translates to interactions

#### Scenario: No endpoint is an error
- GIVEN section has no endpoint set
- WHEN config is loaded
- THEN startup fails with "at least one endpoint must be set"

### Requirement: Per-Endpoint Proxy

Provider sections can specify an explicit proxy for outgoing requests:

```toml
proxy = "http://127.0.0.1:8081"
# or
proxy = "socks5://172.17.0.1:3823"
```

If set, the reqwest `Client` for that section is built with `Proxy::all(url)`. If absent, reqwest falls back to environment variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`).

This applies to ALL handlers (OpenAi, Anthropic, Interactions) — it's a provider-level setting, not interactions-specific. The proxy is configured in `build_app()` when constructing each handler's HTTP client.

#### Scenario: Explicit proxy configured
- GIVEN `proxy = "http://127.0.0.1:8081"` in provider section
- WHEN outgoing requests are sent for that section
- THEN reqwest routes through `http://127.0.0.1:8081`

#### Scenario: No proxy configured
- GIVEN no `proxy` in provider section
- WHEN outgoing requests are sent
- THEN reqwest uses environment proxy variables (if any)

### Requirement: Interactions Auth

When routing to `endpoint_interactions`, the `api_key` config value is sent as `x-goog-api-key` header. Only three headers are sent to the interactions upstream:

| Header | Value |
|--------|-------|
| `x-goog-api-key` | `api_key` from config |
| `Content-Type` | `application/json` |
| `Api-Revision` | `2026-05-20` |

No client request headers are forwarded. The global `forward_request_headers()` function is not used for interactions paths.

#### Scenario: API key as x-goog-api-key
- GIVEN `api_key = "gemini-key-123"`
- WHEN request is sent to interactions endpoint
- THEN `x-goog-api-key: gemini-key-123` header is set

#### Scenario: Client headers NOT forwarded
- GIVEN client sent `Authorization: Bearer client-token` and `x-request-id: abc`
- WHEN routing to interactions endpoint
- THEN neither header appears in the upstream request

### Requirement: Interactions Control Messages

Provider sections with `endpoint_interactions` can optionally specify control message constants:

```toml
control_clean_all = "***!___!--- очисти все сессии gemini interactions ---!___!***"
control_extend_lifetime = "***!___!--- текущую сессию gemini interactions храни до <unix_utc> ---!___!***"
```

Both are optional (if absent, that control feature is disabled). `<unix_utc>` in `control_extend_lifetime` is a placeholder replaced with the actual UTC unix timestamp at runtime.

#### Scenario: Control messages configured
- GIVEN section has `control_clean_all` and `control_extend_lifetime` set
- WHEN config is loaded
- THEN both constants are available for runtime control message detection

#### Scenario: Control messages absent
- GIVEN section has no `control_clean_all` or `control_extend_lifetime`
- WHEN config is loaded
- THEN in-band control is disabled (no messages intercepted)

### Requirement: Interactions Egress Limit (proxy_limit)

Provider sections can specify `proxy_limit` — maximum byte size of the serialized `Content[]` array before splitting into multiple interactions. Uses the same suffix notation as `max_request_body`: `k`, `m`, `g`.

```toml
proxy_limit = "130k"
```

If absent, no splitting occurs (unlimited). When the serialized `Content[]` exceeds the limit:
- Split into chunks, each under the limit
- Chain via `previous_interaction_id`
- Store the LAST chunk's interaction ID in session state
- `message_count` reflects total messages across all chunks

If a single `Content` element alone exceeds `proxy_limit` → error.

#### Scenario: proxy_limit configured
- GIVEN `proxy_limit = "130k"`
- WHEN config is loaded
- THEN the value is parsed as 130 * 1024 bytes

#### Scenario: proxy_limit absent
- GIVEN no `proxy_limit` in section
- WHEN config is loaded
- THEN no size-based splitting is performed (unlimited)

### Requirement: Session Persistence Config

Session state is persisted to a TOML file. Default path by platform:

| Platform | Default |
|----------|---------|
| Linux (.deb) | `/var/lib/inf-splitter/interactions-sessions.toml` |
| Windows | `%ProgramData%\inf-splitter\interactions-sessions.toml` |

Override via global config key:
```toml
interactions_session_store = "/var/lib/inf-splitter/interactions-sessions.toml"
```

This is a runtime data file (not config) — it follows standard OS application data directory conventions. macOS support is out of scope (default path targets Linux and Windows only).

#### Scenario: Linux deb install default
- GIVEN proxy installed from `.deb`
- WHEN no `interactions_session_store` is set
- THEN sessions are persisted to `/var/lib/inf-splitter/interactions-sessions.toml`

#### Scenario: Custom path
- GIVEN `interactions_session_store = "/custom/path/sessions.toml"`
- WHEN proxy starts
- THEN sessions are read from and written to that path

---

## MODIFIED

### Requirement: Provider Sections

Each TOML section (except `[defaults]` and `[diagnostics]`) represents a provider. At least one of `endpoint_openai`, `endpoint_anthropic`, or `endpoint_interactions` must be set.

(Added `endpoint_interactions` as a third endpoint option.)

### Requirement: Global Settings

Top-level config keys:
- `interactions_session_store` — optional path to session persistence TOML file (default `config/interactions-sessions.toml`)
- (unchanged: `listen_host`, `listen_port`, `upstream_timeout`, `max_request_body`, `[[error_translation]]`, `[defaults]`, `[diagnostics]`)

---

## REMOVED

(None)
