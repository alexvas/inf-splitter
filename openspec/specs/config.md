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

### Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `INF_SPLITTER_CONFIG` | Path to TOML config file | `config/inf-splitter.toml` |
| `INF_SPLITTER_LISTEN_HOST` | Override `listen_host` from config | `127.0.0.1` |

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

## Requirement: Per-Section drop_fields

Each provider section can specify `drop_fields` — top-level JSON keys removed from the request body before forwarding upstream. Two forms:

**Flat list** — same fields for every model in the section:
```toml
drop_fields = ["thinking", "stream_options"]
```

**Per-model map** — `"all"` provides base fields, model-specific keys add extra fields:
```toml
[deepseek.drop_fields]
all = ["thinking"]
"deepseek-v4-pro" = ["context_management"]
```

`"all"` is a reserved key; model-specific keys are additive (merged with `"all"`, not replacing). Fields are removed after model extraction and token limit injection, before serialization upstream.

### Scenario: Flat list applies to all models
- GIVEN section has `drop_fields = ["thinking"]` and models `["a", "b"]`
- WHEN a request arrives for model `"a"` or `"b"`
- THEN `thinking` is dropped from the outgoing body

### Scenario: Per-model merge with "all"
- GIVEN `[s.drop_fields]` has `all = ["thinking"]` and `"deepseek-v4-pro" = ["context_management"]`
- WHEN a request arrives for model `"deepseek-v4-pro"`
- THEN both `thinking` and `context_management` are dropped
- WHEN a request arrives for another model in the section
- THEN only `thinking` is dropped (from `"all"`)

### Scenario: drop_fields absent is a no-op
- GIVEN section has no `drop_fields` key
- WHEN a request body is forwarded
- THEN all client fields are passed through unchanged

### Scenario: Dropping non-existent field is silent
- GIVEN `drop_fields = ["nonexistent"]`
- WHEN a request body without `nonexistent` is processed
- THEN the body is forwarded unchanged, no error raised

## Requirement: Global Settings

Top-level config keys:
- `upstream_timeout` — HTTP timeout for upstream calls, suffix `s`/`m` (default `5m`)
- `max_request_body` — max incoming body size, suffix `k`/`m` (default `2m`)
- `body_too_large_hint_statuses` — list of HTTP status codes that trigger a "try reducing context" hint on 413 errors (default `[413]`)

### Scenario: Custom timeout
- GIVEN `upstream_timeout = "30s"`
- WHEN an upstream call is made
- THEN the HTTP client times out after 30 seconds

## Requirement: Model Name Validation

When `models` is a list, each entry is validated: whitespace is trimmed and empty strings are rejected. Previously only the single-string form performed this check; list entries with empty or whitespace-only strings silently registered blank routes.

### Scenario: Empty string in model list
- GIVEN `models = ["valid", ""]`
- WHEN config is loaded
- THEN startup fails with `ConfigError::Provider { name, message: "model name must not be empty" }`

### Scenario: Whitespace-only in model list
- GIVEN `models = ["valid", "  "]`
- WHEN config is loaded
- THEN startup fails with the same error

## Requirement: drop_fields Per-Model Key Validation

When `drop_fields` uses the per-model form (`[section.drop_fields]`), each model-specific key (anything except `"all"`) must match a model in the section's `models` list. Unknown keys are a configuration error. Default sections (`models = "default"`) are exempt from this check since they have no concrete model list.

### Scenario: drop_fields references unknown model
- GIVEN `models = ["known-model"]` and `[s.drop_fields] "unknown-model" = ["field"]`
- WHEN config is loaded
- THEN startup fails with `ConfigError::UnknownDropModel { section: "s", model: "unknown-model" }`

### Scenario: drop_fields "all" key is always valid
- GIVEN `models = ["known-model"]` and `[s.drop_fields] all = ["field"]`
- WHEN config is loaded
- THEN no error (all models in section get the base fields)

## Requirement: Defaults Section Strict Parsing

`DefaultConfig` has `#[serde(deny_unknown_fields)]`. Unknown keys in `[defaults]` cause a TOML parse error naming the unrecognized field, preventing silent misconfiguration when an operator puts endpoint fields under `[defaults]`.

### Scenario: Unknown field in defaults
- GIVEN `[defaults]` with `endpoint_openai = "http://x"`
- WHEN config is loaded
- THEN TOML parse fails with "unknown field `endpoint_openai`"
