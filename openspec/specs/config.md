# Spec: Configuration

Component: `src/config.rs`

## Requirement: TOML Configuration File

The proxy reads a single TOML configuration file whose path can be overridden via `INF_SPLITTER_CONFIG` env var. Default path: `config/inf-splitter.toml`.

### Scenario: Default config path
- GIVEN `INF_SPLITTER_CONFIG` is not set
- WHEN the proxy starts
- THEN it reads `config/inf-splitter.toml`

### Scenario: Custom config path
- GIVEN `INF_SPLITTER_CONFIG=/etc/inf-splitter/custom.toml`
- WHEN the proxy starts
- THEN it reads `/etc/inf-splitter/custom.toml`

## Requirement: Listen Address Configuration

The proxy binds to `listen_host`:`listen_port` from the TOML config.

- `listen_host` defaults to `127.0.0.1`
- `listen_port` defaults to `3000`
- Can be overridden via `INF_SPLITTER_LISTEN_HOST`
- No `LISTEN_ADDR` env var exists — host and port are separate

### Scenario: Default listen address
- GIVEN no `listen_host` or `listen_port` in config
- WHEN the proxy starts
- THEN it listens on `127.0.0.1:3000`

### Scenario: Docker listen override
- GIVEN `INF_SPLITTER_LISTEN_HOST=0.0.0.0`
- WHEN the proxy starts
- THEN it listens on `0.0.0.0:{port}`

## Requirement: Provider Sections

Each TOML section (except `[defaults]` and `[diagnostics]`) represents a provider. At least one of `endpoint_openai` or `endpoint_anthropic` must be set.

### Scenario: OpenAI endpoint only
- GIVEN a section with only `endpoint_openai = "http://..."` 
- WHEN a request arrives for a model in that section
- THEN the proxy routes to that OpenAI endpoint (with conversion if ingress is Anthropic)

### Scenario: Anthropic endpoint only
- GIVEN a section with only `endpoint_anthropic = "https://..."`
- WHEN a request arrives for a model in that section
- THEN the proxy routes to that Anthropic endpoint (with conversion if ingress is OpenAI)

### Scenario: Both endpoints set
- GIVEN a section with both `endpoint_openai` and `endpoint_anthropic`
- WHEN requests arrive via `/v1/chat/completions` or `/v1/messages`
- THEN each ingress protocol is routed to its matching endpoint without conversion

## Requirement: Model Routing

Models are matched against provider sections in order of definition. Supported model forms:
- Single string: `models = "gemma4:31b"`
- List: `models = ["deepseek-v4-pro", "deepseek-v4-flash"]`
- Catch-all: `models = "default"` (matches any unmatched model)

### Scenario: Exact model match
- GIVEN section `[deepseek]` has `models = ["deepseek-v4-pro"]`
- WHEN a request arrives with `"model": "deepseek-v4-pro"`
- THEN it routes to the `[deepseek]` section

### Scenario: Default fallback
- GIVEN section `[etc]` has `models = "default"`
- WHEN a request arrives with an unmatched model name
- THEN it routes to the `[etc]` section

### Scenario: First-match ordering
- GIVEN two sections have `models = "default"` 
- WHEN a request arrives with an unmatched model
- THEN the first section in config file order wins

## Requirement: Secret Resolution

`api_key` values support `${VAR}` substitution resolved in order:
1. Environment variable `VAR`
2. File `secrets/VAR` (first line, trimmed)

### Scenario: Env var takes precedence
- GIVEN `api_key = "${DEEPSEEK_API_KEY}"` and both env var and `secrets/DEEPSEEK_API_KEY` exist
- WHEN resolving the secret
- THEN the environment variable value is used

### Scenario: Missing secret
- GIVEN `api_key = "${MISSING_KEY}"` and neither env var nor file exists
- WHEN resolving the secret
- THEN startup fails with "secret not found" error

## Requirement: Token Limits

`[defaults]` section provides global token limits. Per-section limits override defaults. Supported fields:
- `max_tokens` — applies to both protocols
- `max_output_tokens` — OpenAI non-standard field (JSON passthrough only)
- `max_completion_tokens` — OpenAI standard field (JSON passthrough only)

### Scenario: Per-section override
- GIVEN `[defaults]` has `max_tokens = 4096` and `[deepseek]` has `max_tokens = 8192`
- WHEN a request routes to deepseek
- THEN `max_tokens` is capped at 8192

### Scenario: No limit configured
- GIVEN no `[defaults]` and no per-section `max_tokens`
- WHEN a request is processed
- THEN no token limit is injected

## Requirement: Global Settings

Top-level config keys:
- `upstream_timeout` — HTTP timeout for upstream calls, suffix `s`/`m` (default `5m`)
- `max_request_body` — max incoming body size, suffix `k`/`m` (default `2m`)
- `body_too_large_hint_statuses` — list of HTTP status codes that trigger a "try reducing context" hint on 413 errors (default `[413]`)

### Scenario: Custom timeout
- GIVEN `upstream_timeout = "30s"`
- WHEN an upstream call is made
- THEN the HTTP client times out after 30 seconds
