# Agent guidelines for inf-splitter

Mandatory checks before opening or merging a PR. These mirror [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Required checks

**After any Rust code change, run `cargo fmt` before committing.** Then verify:

Run from the repository root:

```bash
cargo fmt --check
cargo clippy --locked -- -D warnings
cargo test --locked
```

All three must pass. Do not merge if any step fails.

## Optional local checks

```bash
./scripts/docker-smoke-test.sh
```

Use after Docker-related changes or before release.

## Scope notes

- Ingress is **no-auth** by design; do not add ingress authentication without an explicit product decision.
- Default listen address is **`0.0.0.0:{port}`** from TOML; do not reintroduce `LISTEN_ADDR`.
- Timeout and request body limits are configured in TOML with suffixes (`15s`, `1m`, `512k`, `2m`).
