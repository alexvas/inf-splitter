# Spec: Deployment & Packaging

Components: `Dockerfile`, `debian/`, `packaging/windows/`, `scripts/`

## Requirement: Docker Deployment

The project builds a minimal Docker image. Key design decisions:
- `INF_SPLITTER_LISTEN_HOST=0.0.0.0` required for Docker networking
- Config and secrets mounted read-only via volumes
- Stats to `stderr` appear in `docker logs`

### Scenario: Docker build
- GIVEN the Dockerfile
- WHEN `docker build -t inf-splitter .` is run
- THEN a working image is produced

### Scenario: Docker run
- GIVEN config and secrets directories exist
- WHEN `docker run --rm -v $PWD/config:/app/config:ro -v $PWD/secrets:/app/secrets:ro -e INF_SPLITTER_LISTEN_HOST=0.0.0.0 inf-splitter`
- THEN the proxy starts and listens on port 3000

### Scenario: Host Ollama access from Docker
- GIVEN Ollama runs on host at port 11434
- WHEN Docker container uses `http://host.docker.internal:11434` as endpoint
- THEN `extra_hosts: host.docker.internal:host-gateway` in compose resolves the address

## Requirement: Linux Package (.deb)

The `.deb` package installs:
- Binary: `/usr/bin/inf-splitter`
- Config: `/etc/inf-splitter/inf-splitter.toml`
- Env template: `/etc/inf-splitter/environment`
- Log directory: `/var/log/inf-splitter` (owned by `inf-splitter:inf-splitter`)
- Session directory: `/var/lib/inf-splitter` (owned by `inf-splitter:inf-splitter`)
- systemd unit: enabled and started on install

The systemd unit allows writes to `/var/log/inf-splitter` and `/var/lib/inf-splitter` via `ReadWritePaths` so the service can persist session state at runtime under `ProtectSystem=strict`.

### Scenario: Fresh install
- GIVEN `dpkg -i inf-splitter_*.deb` is run
- THEN the service is installed, enabled, and running
- AND `/var/lib/inf-splitter/` exists with owner `inf-splitter:inf-splitter`

### Scenario: Session directory survives upgrade
- GIVEN `dpkg -i inf-splitter_*.deb` is run on a system where `/var/lib/inf-splitter/` already exists
- WHEN postinst executes
- THEN `mkdir -p` is a no-op
- AND existing session data is preserved

### Scenario: Post-install config
- GIVEN the service is installed
- WHEN `/etc/inf-splitter/inf-splitter.toml` is edited and the service restarted
- THEN the new config takes effect

### Scenario: Log access
- WHEN `journalctl -u inf-splitter -f` is run
- THEN service logs (including stderr diagnostics) are displayed

## Requirement: Windows Package (zip)

The `inf-splitter-windows.zip` artifact contains the binary, config, and `install.ps1`:
- Installs to `%ProgramData%\inf-splitter\`
- Creates `secrets\` directory for API key files
- Creates and starts a Windows service via WinSW
- Config: `%ProgramData%\inf-splitter\config.toml`

Secrets are stored as files in `%ProgramData%\inf-splitter\secrets\`: one file per key, filename = variable name, content = key value. The `${VAR}` resolution in config reads `secrets/VAR` on all platforms. Env vars (via WinSW) take precedence when both exist.

### Scenario: Windows install creates secrets dir
- GIVEN `install.ps1` is run as Administrator
- WHEN installation completes
- THEN `%ProgramData%\inf-splitter\secrets\` exists and is ready for API keys

### Scenario: API key from secrets file
- GIVEN `config.toml` has `api_key = "${DEEPSEEK_API_KEY}"`
- WHEN `%ProgramData%\inf-splitter\secrets\DEEPSEEK_API_KEY` contains `sk-abc123`
- THEN the proxy uses `sk-abc123` as the API key

### Scenario: Windows install
- GIVEN the zip is extracted
- WHEN `install.ps1` is run as Administrator
- THEN the service is created and started

### Scenario: Windows config change
- GIVEN the service is running
- WHEN `%ProgramData%\inf-splitter\config.toml` is edited and the service restarted
- THEN the new config takes effect

## Requirement: Docker Smoke Test

`scripts/docker-smoke-test.sh` validates:
1. Docker image builds successfully
2. Container starts with mounted config
3. HTTP endpoints respond correctly
4. Configurable image tag via `SMOKE_IMAGE` env var

### Scenario: Smoke test pass
- GIVEN the Docker context is valid
- WHEN `./scripts/docker-smoke-test.sh` is run
- THEN it exits 0 if all checks pass

## Requirement: CI Artifacts

CI produces these artifacts on every push to main:
- `inf-splitter_*.deb` (Linux, amd64)
- `inf-splitter-windows.zip` (Windows, amd64)
- Docker image (tagged)

## Requirement: HTTP Client Compression

The reqwest HTTP client used for all egress requests to upstream providers supports the following content encodings:

| Feature | Algorithm | RFC |
|---------|-----------|-----|
| `gzip` | GZIP | RFC 1952 |
| `deflate` | DEFLATE | RFC 1951 |
| `brotli` | Brotli | RFC 7932 |
| `zstd` | Zstandard | RFC 8878 |

When these features are enabled, reqwest automatically:
- Advertises supported algorithms in the `Accept-Encoding` request header
- Transparently decompresses upstream response bodies

No code-level changes are needed — reqwest handles advertisement and decompression internally.

### Scenario: Upstream returns brotli-compressed response
- GIVEN an upstream returns `Content-Encoding: br`
- WHEN the proxy receives the response
- THEN reqwest transparently decompresses it before the proxy processes the body

### Scenario: Upstream returns zstd-compressed response
- GIVEN an upstream returns `Content-Encoding: zstd`
- WHEN the proxy receives the response
- THEN reqwest transparently decompresses it before the proxy processes the body

### Scenario: Accept-Encoding advertisement
- GIVEN the proxy is built with all compression features
- WHEN an egress request is sent upstream
- THEN the `Accept-Encoding` header includes `gzip, br, zstd, deflate`

### Scenario: Gzip fallback still works
- GIVEN an upstream only supports `gzip`
- WHEN the proxy sends a request
- THEN the upstream returns `Content-Encoding: gzip` and reqwest decompresses it as before
