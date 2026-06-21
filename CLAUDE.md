# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & test

```bash
cargo fmt --check              # formatting
cargo clippy --locked -- -D warnings  # lint
cargo test --locked            # all tests (unit + integration)
cargo test -p inf-splitter -- test_name  # single test
./scripts/docker-smoke-test.sh # Docker integration smoke test
```

All three checks (`fmt`, `clippy`, `test`) must pass before merging. CI runs them on every push to main and every PR.

## Architecture

inf-splitter is an HTTP proxy that routes LLM inference requests to OpenAI- and Anthropic-compatible upstreams based on model name from a TOML config. It handles protocol conversion between OpenAI and Anthropic formats via `anyllm_translate`.

### Request flow

```
Client → POST /v1/chat/completions  or  /v1/messages
       → router::dispatch_messages:
           1. Peek `model` field from JSON body
           2. Config::resolve_route(&model) → RouteTarget
           3. Match ingress protocol against available endpoints:
              - endpoint_openai is set → passthrough via OpenAiHandler
              - only endpoint_anthropic → translate via AnthropicHandler
              - endpoint_interactions → translate via InteractionsHandler
              - (and vice versa for Anthropic ingress)
       → Handler sends request upstream, translates response if needed
```

### Module roles

| Module | Role |
|--------|------|
| `config.rs` | TOML parsing, model→section routing, secret resolution (`${VAR}` from env or `secrets/VAR`), duration/byte-size parsing (`15s`, `2m`, `512k`) |
| `router.rs` | Axum routes, `AppState`, `/health` readiness probe (parallel upstream checks, 5s cache, GET for interactions), `/openai/v1/models` + `/anthropic/v1/models`, `/interactions/v1/control-constants`, dispatch logic |
| `openai.rs` | `OpenAiHandler` — sends to `/v1/chat/completions`, handles Anthropic→OpenAI conversion via `anyllm_translate` and direct HTTP |
| `anthropic.rs` | `AnthropicHandler` — sends to `/v1/messages`, handles OpenAI→Anthropic conversion |
| `interactions_handler.rs` | `InteractionsHandler` — translates Anthropic/OpenAI ingress to Gemini Interactions API (`/v1beta/interactions`), session-aware delta computation, `proxy_limit` content splitting with system instruction chunking, control message handling |
| `interactions.rs` | Interactions request/response translation helpers: Anthropic/OpenAI message extraction, `split_content_for_limit`, `single_element_too_large` |
| `interactions_types.rs` | `include!` of build-time generated serde types from `schemas/interactions.openapi.json` |
| `session.rs` | `SessionStore` — persistent session state (TOML file), TTL eviction, delta computation, startup recovery with pending verification |
| `control.rs` | Control message scanning (`scan_control_messages`), stripping, idempotency via hash tracking |
| `sse.rs` | Shared SSE utilities: event-stream detection, line parsing, event formatting |
| `auth.rs` | `forward_request_headers()` — forwards non-hop-by-hop headers to upstream, applies auth override when `api_key` is set |
| `error.rs` | `AppError` → Anthropic-format JSON error response (`{"type":"error","error":{...}}`) |
| `lib.rs` | `build_app()` — wires up handlers, body limit layer, 413→JSON middleware with hint |

### Config model (TOML)

Top-level: `listen_host`, `listen_port`, `upstream_timeout`, `max_request_body`, optional `[[error_translation]]` (array of tables: `status`, optional `ingress`, `egress`), optional `[defaults]`.

Each provider section has:
- `endpoint_openai` / `endpoint_anthropic` / `endpoint_interactions` — at least one required; determines routing direction
- `models` — single string, list, or `"default"` (catch-all)
- `api_key`, `max_tokens`, `max_output_tokens`, `max_completion_tokens` — optional
- `proxy` — optional HTTP/SOCKS5 proxy URL for outgoing requests from this section
- `proxy_limit` — optional byte-size limit for interactions content splitting (e.g. `"130k"`)
- `control_clean_all`, `control_extend_lifetime` — optional control message trigger strings for interactions sessions

Per-section token limits override `[defaults]`, which override nothing (no limit applied).

### Token limits injection

`max_tokens`, `max_output_tokens`, `max_completion_tokens` are injected into outgoing requests:
- For passthrough paths: the JSON body is parsed, `cap_numeric_field()` clamps or sets the field
- For conversion paths: the typed request struct (`ChatCompletionRequest`, `MessageCreateRequest`) is mutated before sending
- `max_output_tokens` only applies via JSON passthrough (the typed Anthropic struct has only `max_tokens`)

### Protocol conversion

When ingress protocol doesn't match the available upstream endpoint, `anyllm_translate` converts:
- Anthropic ingress → OpenAI upstream: `MessageCreateRequest` → `ChatCompletionRequest`, response translated back
- OpenAI ingress → Anthropic upstream: `ChatCompletionRequest` → `MessageCreateRequest` (Anthropic format), response translated back

Both streaming and non-streaming paths are handled. `stream_options` is always dropped from OpenAI requests (hardcoded).

### Health check

`GET /health` probes each unique upstream endpoint with a HEAD request (2s timeout per check, 5s result cache). Returns `{"status":"ok","upstreams":{...}}` or `{"status":"degraded",...}` with HTTP 503.

## Scope constraints

- Serialization and deserialization must use strict type-checked structs from the protocol schemas whenever possible — never raw `json!()` or string snippets when a typed constructor or serde struct exists. The generated types in `crate::interactions_types` (from `schemas/interactions.openapi.json` via `build.rs`) include `CreateModelInteractionParams`, `InteractionsInput`, `GenerationConfig`, and all response/event types. Use `..Default::default()` for ergonomic construction of structs with many optional fields. Manual `impl Default` is added in `src/interactions_types.rs` for key types; add new ones there as needed.
- Parse ingress JSON into typed structs at the protocol boundary. Pass typed values down the call stack. Avoid threading raw `serde_json::Value` through functions when typed equivalents exist. For the interactions pipeline, `build_interactions_request_anthropic`/`build_interactions_request_openai` accept typed scalars and `&[Value]` messages (not a raw body Value), and return `CreateModelInteractionParams` (not a generic Value). Split-path logic accesses struct fields directly, not `.get("key")`.
- Ingress is **no-auth** by design; do not add authentication without explicit product decision
- Default listen address is `127.0.0.1:{port}` from TOML (no `LISTEN_ADDR` env var)
- Limits use suffix notation: `s`/`m` for durations, `k`/`m` for byte sizes
- The `openssl` crate uses `vendored` feature — compiles OpenSSL from source, no system `libssl-dev` needed
- **READMEs are trilingual.** Any change to one README (`README.md`, `README.en.md`, `README.zh.md`) must be reflected in all three. Keep structure (headings) and content in sync across languages. Pre-commit hook validates heading counts.
