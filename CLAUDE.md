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
Client → POST /openai/v1/messages  or  /anthropic/v1/messages
       → router::dispatch_messages:
           1. Peek `model` field from JSON body
           2. Config::resolve_route(&model) → RouteTarget
           3. Match ingress protocol against available endpoints:
              - endpoint_openai is set → passthrough via OpenAiHandler
              - only endpoint_anthropic → translate via AnthropicHandler
              - (and vice versa for Anthropic ingress)
       → Handler sends request upstream, translates response if needed
```

### Module roles

| Module | Role |
|--------|------|
| `config.rs` | TOML parsing, model→section routing, secret resolution (`${VAR}` from env or `secrets/VAR`), duration/byte-size parsing (`15s`, `2m`, `512k`) |
| `router.rs` | Axum routes, `AppState`, `/health` readiness probe (parallel upstream checks, 5s cache), `/openai/v1/models` + `/anthropic/v1/models`, dispatch logic |
| `openai.rs` | `OpenAiHandler` — sends to `/v1/chat/completions`, handles Anthropic→OpenAI conversion via `anyllm_translate` and direct HTTP |
| `anthropic.rs` | `AnthropicHandler` — sends to `/v1/messages`, handles OpenAI→Anthropic conversion |
| `sse.rs` | Shared SSE utilities: event-stream detection, line parsing, event formatting |
| `auth.rs` | `forward_request_headers()` — forwards non-hop-by-hop headers to upstream, applies auth override when `api_key` is set |
| `error.rs` | `AppError` → Anthropic-format JSON error response (`{"type":"error","error":{...}}`) |
| `lib.rs` | `build_app()` — wires up handlers, body limit layer, 413→JSON middleware with hint |

### Config model (TOML)

Top-level: `listen_host`, `listen_port`, `upstream_timeout`, `max_request_body`, `body_too_large_hint_statuses` (default `[413]`), optional `[defaults]`.

Each provider section has:
- `endpoint_openai` / `endpoint_anthropic` — at least one required; determines routing direction
- `models` — single string, list, or `"default"` (catch-all)
- `api_key`, `max_tokens`, `max_output_tokens`, `max_completion_tokens` — optional

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

- Ingress is **no-auth** by design; do not add authentication without explicit product decision
- Default listen address is `127.0.0.1:{port}` from TOML (no `LISTEN_ADDR` env var)
- Limits use suffix notation: `s`/`m` for durations, `k`/`m` for byte sizes
- The `openssl` crate uses `vendored` feature — compiles OpenSSL from source, no system `libssl-dev` needed
