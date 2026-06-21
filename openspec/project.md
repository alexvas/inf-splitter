# Project: inf-splitter

HTTP proxy that routes LLM inference requests to OpenAI- and Anthropic-compatible upstreams based on model name from a TOML config.

## Tech Stack

- **Language:** Rust (edition 2021)
- **Web framework:** Axum 0.8 + Tokio
- **HTTP client:** reqwest 0.12 (rustls-tls-native-roots, gzip, SOCKS proxy support)
- **Protocol translation:** anyllm_translate 0.9
- **Config format:** TOML
- **Serialization:** serde + serde_json
- **Compression:** flate2, zip
- **License:** GPL-3.0-or-later

## Build & Test

```bash
cargo fmt --check              # formatting
cargo clippy --locked -- -D warnings  # lint
cargo test --locked            # all tests (unit + integration)
cargo test -p inf-splitter -- test_name  # single test
./scripts/docker-smoke-test.sh # Docker integration smoke test
```

All three checks (`fmt`, `clippy`, `test`) must pass before merging. CI runs them on every push to main and every PR.

## Conventions

- Ingress is **no-auth** by design — auth is handled at the upstream boundary only
- Default listen address is `127.0.0.1:{port}` from TOML (no `LISTEN_ADDR` env var)
- Limits use suffix notation: `s`/`m` for durations, `k`/`m` for byte sizes
- The `openssl` crate uses `vendored` feature — compiles OpenSSL from source
- **READMEs are trilingual** — any change to one must be reflected in all three (`README.md`, `README.en.md`, `README.zh.md`). Pre-commit hook validates heading counts.
- Diagnostics timestamps use ISO 8601 UTC format (`YYYY-MM-DDTHH:MM:SSZ`)
- Secret resolution order: environment variable → `secrets/VAR` file

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| anyllm_translate | OpenAI↔Anthropic protocol conversion |
| axum | HTTP server framework |
| reqwest | HTTP client (rustls, gzip, SOCKS) |
| tower-http | Body limiting, tracing middleware |
| base64 | Non-UTF8 body encoding in dumps |
| flate2, zip | Diagnostic file compression |

## Scope Constraints

- No authentication on ingress (by design, not accidental)
- No admin UI or management endpoints beyond `/health`
- No LiteLLM YAML compatibility layer
- OS packaging: Linux (.deb via systemd), Windows (zip via WinSW)
- Docker is a supported deployment target
