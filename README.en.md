[ Русский ](README.md) | **English** | [ 中文 ](README.zh.md)

# inf-splitter

A thin HTTP router for inference requests: model-based routing to OpenAI- and Anthropic-compatible upstreams from a TOML config.

**Primary use case — running on localhost.** The service listens on `127.0.0.1:{port}` by default (port from TOML, default 3000).

Replaces `anyllm-proxy`: no LiteLLM YAML, admin UI, or SSRF bypasses via `/etc/hosts`.

## Ingress security (no-auth)

**Incoming requests to the proxy are not authenticated.** Any client on the network with access to the service port can send requests. The operator is responsible for perimeter security (network, reverse proxy, firewall).

Authentication applies only on the upstream provider side: if a config section specifies `api_key`, the proxy injects it into the upstream request; if `api_key` is absent, the client's incoming auth headers are forwarded as-is.

## Configuration

Main config file: [`config/inf-splitter.toml`](config/inf-splitter.toml).

```toml
upstream_timeout = "3m"
max_request_body = "2m"

[defaults]
max_tokens = 4096
max_completion_tokens = 8192

[ollama]
endpoint_openai = "http://127.0.0.1:11434"
models = "gemma4:31b"

[deepseek]
endpoint_anthropic = "https://api.deepseek.com/anthropic"
api_key = "${DEEPSEEK_API_KEY}"
models = ["deepseek-v4-pro[1m]", "deepseek-v4-flash"]

[etc]
endpoint_openai = "https://api.modelarts-maas.com/openai/v1"
api_key = "${MAAS_API_KEY}"
models = "default"
```

| Field | Description |
|-------|-------------|
| `listen_host` | IP address for incoming connections (default `127.0.0.1`; for Docker — `0.0.0.0`) |
| `listen_port` | TCP port (default 3000) |
| `upstream_timeout` | Timeout for outgoing upstream requests; suffixes `s` (seconds) or `m` (minutes), e.g. `15s`, `1m` (default `5m`) |
| `max_request_body` | Max incoming request body size; suffixes `k` (KiB) or `m` (MiB), e.g. `512k`, `2m` (default `2m`) |
| `body_too_large_hint_statuses` | Optional list of HTTP status codes (integers) for which a `Try reducing context size...` hint is appended to the error (default `[413]`, empty list = no hint) |

### `[defaults]` section

Global token limits for all providers. Individual providers can override these.

| Field | Description |
|-------|-------------|
| `max_tokens` | Global `max_tokens` limit (applies to all upstreams unless overridden) |
| `max_output_tokens` | Global `max_output_tokens` limit (Anthropic/Gemini-compatible upstreams) |
| `max_completion_tokens` | Global `max_completion_tokens` limit (OpenAI-compatible upstreams) |

### Provider sections

| Field | Description |
|-------|-------------|
| `endpoint_openai` | Optional; base URL of an OpenAI-compatible upstream. When set, incoming `/openai` requests go here without conversion |
| `endpoint_anthropic` | Optional; base URL of an Anthropic-compatible upstream. When set, incoming `/anthropic` requests go here without conversion |
| `models` | A single model, a list of models, or `"default"` (fallback for unmatched models) |
| `api_key` | Optional; `${VAR}` resolves from the environment or `secrets/VAR` file |
| `max_tokens` | Optional; caps `max_tokens` in the outgoing request. If the client omits or exceeds it, the proxy injects the limit |
| `max_output_tokens` | Optional; caps `max_output_tokens` (Anthropic/Gemini-compatible upstreams) |
| `max_completion_tokens` | Optional; caps `max_completion_tokens` (OpenAI-compatible upstreams) |

The config path can be overridden via `INF_SPLITTER_CONFIG`.

### Environment variables

| Variable | Description |
|----------|-------------|
| `INF_SPLITTER_CONFIG` | Path to the TOML config (default `config/inf-splitter.toml`) |
| `INF_SPLITTER_LISTEN_HOST` | IP address for incoming connections (default `127.0.0.1`; for Docker — `0.0.0.0`) |

### Secrets

```bash
mkdir -p secrets
cp secrets.example/* secrets/
# edit secrets/DEEPSEEK_API_KEY, secrets/MAAS_API_KEY
```

The `secrets/` directory is in `.gitignore` — never commit real keys.

`${VAR}` resolution order: environment variable → `secrets/VAR` file.

## Routing

```
Claude Code  --POST /openai/v1/messages-->     inf-splitter
            --POST /anthropic/v1/messages-->
                         |
              model + ingress protocol
                         |
         +---------------+---------------+
         |                               |
    OPENAI section                  ANTHROPIC section
         |                               |
    OpenAI upstream               Anthropic upstream
  (/v1/chat/completions)           (/v1/messages)
```

| Model | Section | Recommended ingress |
|-------|---------|---------------------|
| `gemma4:31b` | `[ollama]` | `POST /openai/v1/messages` |
| `deepseek-v4-pro[1m]`, `deepseek-v4-flash` | `[deepseek]` | `POST /anthropic/v1/messages` |
| any other | `[etc]` (`default`) | `POST /openai/v1/messages` |

The ingress endpoint specifies the **incoming request format and client response format**. The TOML section specifies the **target upstream** via `endpoint_openai` and/or `endpoint_anthropic`. When both are set, `/openai` goes to `endpoint_openai`, `/anthropic` goes to `endpoint_anthropic` (passthrough). When only one is set, the opposite ingress is converted via `anyllm_translate`.

| Ingress | Endpoint availability | Behavior |
|---------|----------------------|----------|
| `/openai/v1/messages` | `endpoint_openai` set | passthrough → OpenAI upstream |
| `/openai/v1/messages` | only `endpoint_anthropic` | OpenAI → Anthropic → OpenAI |
| `/anthropic/v1/messages` | `endpoint_anthropic` set | passthrough → Anthropic upstream |
| `/anthropic/v1/messages` | only `endpoint_openai` | Anthropic → OpenAI → Anthropic |

### API keys

| Section | `api_key` | Behavior |
|---------|-----------|----------|
| `[ollama]` | not set | Client's incoming key (Ollama ignores Authorization) |
| `[deepseek]` | `${DEEPSEEK_API_KEY}` | Proxy injects key from env/`secrets/` |
| `[etc]` | `${MAAS_API_KEY}` | Proxy injects key from env/`secrets/` |

### `[diagnostics]` section (optional)

Controls statistics collection and request/response dumps. Writes NDJSON lines to the specified sink. Everything is off by default.

```toml
[diagnostics]
# Where to write NDJSON stats: "stderr" (default), "stdout", or a file path.
stats_output = "stderr"

# Where to write NDJSON dumps: "stderr" (default), "stdout", or a file path.
dump_output = "/app/logs/dump.ndjson"

# Stats (per-request summary: model, duration, token count, message breakdown):
# "off" — disabled; "error" — only on errors; "all" — every request.
stats_mode = "error"

# Request/response body dumps (for debugging; can be large):
# "off" — disabled; "error" — only on errors; "all" — every request.
dump_mode = "off"

# Flush interval (optional, e.g. "10s", "1m").
# When absent, flushes after every line. Useful for file output
# to reduce disk I/O.
flush_period = "10s"
```

When running in Docker with `stats_output = "stderr"`, stats lines appear in `docker logs`. To write to a file, mount a volume (`- ./logs:/app/logs`) and set `stats_output = "/app/logs/diagnostics.ndjson"`. Same for `dump_output`.

## HTTP API

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Readiness probe: `{"status":"ok","upstreams":{...}}` or `{"status":"degraded",...}` (HTTP 503) when upstreams are unavailable |
| `GET` | `/openai/v1/models` | OpenAI-compatible model list |
| `GET` | `/anthropic/v1/models` | Anthropic-compatible model list |
| `POST` | `/openai/v1/messages` | OpenAI format; upstream resolved by `model` from TOML |
| `POST` | `/anthropic/v1/messages` | Anthropic format; upstream resolved by `model` from TOML |

### `GET /openai/v1/models` and `GET /anthropic/v1/models`

Return all model IDs explicitly listed in TOML (excluding `"default"`), in lexicographic order.

## docker-compose integration

The `Claude CLI` agent uses the router as an upstream Anthropic API:

- `ANTHROPIC_BASE_URL=http://inf-splitter:${PROXY_PORT:-3000}/anthropic` (within the network)
- For local models via OpenAI protocol: `http://inf-splitter:${PROXY_PORT}/openai`

Mount your config and secrets. For Docker, set `INF_SPLITTER_LISTEN_HOST=0.0.0.0`:

```yaml
environment:
  - INF_SPLITTER_LISTEN_HOST=0.0.0.0
volumes:
  - ./inf-splitter/config:/app/config:ro
  - ./inf-splitter/secrets:/app/secrets:ro
```

### Host access to Ollama

In Docker, use `http://host.docker.internal:11434` for `[ollama].endpoint` and `extra_hosts: host.docker.internal:host-gateway` in compose.

## Build & run

### Local (cargo)

```bash
cd inf-splitter
cp secrets.example/* secrets/
export DEEPSEEK_API_KEY=sk-...   # or put keys in secrets/
export MAAS_API_KEY=sk-...
cargo run
```

### Docker

```bash
docker build -t inf-splitter .
docker run --rm \
  -v "$PWD/config:/app/config:ro" \
  -v "$PWD/secrets:/app/secrets:ro" \
  inf-splitter
```

## Releases

Pre-built packages are available in [GitHub Releases](https://github.com/) (CI artifacts for every push to `main`).

### Linux (.deb)

```bash
sudo dpkg -i inf-splitter_*.deb
```

The package installs the binary to `/usr/bin/inf-splitter`, config to `/etc/inf-splitter/inf-splitter.toml`, environment variable template to `/etc/inf-splitter/environment`, and a systemd service.

After installation:
1. Edit `/etc/inf-splitter/inf-splitter.toml` — configure your upstreams
2. Fill `/etc/inf-splitter/environment` — set API keys (format `VAR=value`, one per line)
3. The service is already running: `systemctl status inf-splitter`

```bash
# After changing config or environment variables:
sudo systemctl restart inf-splitter

# Logs:
journalctl -u inf-splitter -f
```

### Windows (zip)

Download `inf-splitter-windows.zip` from artifacts, unzip, and run `install.ps1` as Administrator:

```powershell
Expand-Archive inf-splitter-windows.zip -DestinationPath C:\temp\inf-splitter
cd C:\temp\inf-splitter\inf-splitter
.\install.ps1
```

The script creates `%ProgramData%\inf-splitter\`, installs and starts the Windows service.

After installation:
1. Edit `%ProgramData%\inf-splitter\config.toml`
2. Set API keys via WinSW: `& "$env:ProgramData\inf-splitter\inf-splitter-service.exe" set VAR=value`
3. Restart the service: `Restart-Service inf-splitter`

```powershell
Get-Service inf-splitter          # service status
Get-EventLog -LogName Application -Source inf-splitter  # logs
```

## Code structure

```
src/
├── main.rs      # entry point, graceful shutdown
├── config.rs    # TOML, model/default routing, secrets
├── auth.rs      # api_key injection / auth header forwarding
├── router.rs    # axum routes, /v1/models (openai+anthropic), /health
├── openai.rs    # OpenAI upstream + Anthropic↔OpenAI conversion
├── anthropic.rs # Anthropic upstream + OpenAI↔Anthropic conversion
├── sse.rs       # shared SSE utilities (parsing, formatting, responses)
└── error.rs     # errors in Anthropic API format
```

## Tests

```bash
env -u RUSTUP_TOOLCHAIN cargo test
```

Protocol conversion integration tests: `tests/protocol_conversion.rs` (mock upstream + HTTP via proxy).

### Docker smoke test

Validates image build, startup with mounted config, and HTTP endpoints:

```bash
./scripts/docker-smoke-test.sh
```

Variables: `SMOKE_IMAGE` (image tag, default `inf-splitter:smoke-test`).

## Troubleshooting

- **Config load failed: secret not found** — set the env variable or copy `secrets.example/` to `secrets/`.
- **llama: Connection refused** — check `[llama-local].endpoint` and llama accessibility from localhost.

## License

This project is distributed under the [GNU General Public License v3.0 or later](LICENSE) (GPL-3.0-or-later).

Rust dependencies are listed in [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES); common license texts are in the [licenses/](licenses/) directory. CI validates this file on every push. When updating `Cargo.lock`, regenerate the list:

```bash
python3 scripts/generate-third-party-notices.py
```
