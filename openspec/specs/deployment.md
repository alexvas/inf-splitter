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
- systemd unit: enabled and started on install

### Scenario: Fresh install
- GIVEN `dpkg -i inf-splitter_*.deb` is run
- THEN the service is installed, enabled, and running

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
- Creates and starts a Windows service via WinSW
- Config: `%ProgramData%\inf-splitter\config.toml`

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
