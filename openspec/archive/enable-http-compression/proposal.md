# Proposal: Enable Brotli, Zstd & Deflate Compression

**Change ID:** `enable-http-compression`
**Created:** 2026-06-22
**Status:** Implemented

---

## Problem Statement

The proxy's reqwest HTTP client currently only supports `gzip` content encoding for egress requests to upstream providers. Claude CLI (and potentially other modern clients) sends:

```
accept-encoding: gzip, deflate, br, zstd
```

Without `brotli`, `zstd`, and `deflate` features enabled, reqwest:
- Does **not** advertise these algorithms in `Accept-Encoding` headers to upstreams
- Cannot decompress upstream responses compressed with `br` or `zstd`
- Falls back to uncompressed transfer, wasting bandwidth on large response bodies

The proxy's `gzip` feature already proves the value — the diagnostics spec explicitly documents that gzip-compressed Gemini error responses are auto-decompressed. Enabling the remaining algorithms extends this benefit to all common encodings.

## Proposed Solution

Enable three additional reqwest features in `Cargo.toml`:
- `brotli` — Brotli (RFC 7932), commonly used by CDNs and modern APIs
- `zstd` — Zstandard (RFC 8878), high-ratio compression with low CPU cost
- `deflate` — DEFLATE (RFC 1951), legacy but still in active use

Reqwest automatically handles advertisement and decompression when these features are enabled — no code changes required.

## Scope

### In Scope
- `Cargo.toml`: add `brotli`, `zstd`, `deflate` to reqwest features

### Out of Scope
- Manual `Accept-Encoding` header manipulation (reqwest handles it)
- Per-upstream compression configuration (no use case identified)
- Diagnostic file compression (already supports zip, bz2, 7z)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| Cargo.toml | Yes | 3 new reqwest features |
| Code | No | Automatic |
| Config | No | — |
| Tests | No | Existing tests validate no regressions |

## Success Criteria

- [x] `cargo check` passes with new features
- [x] `cargo test` — all 308 tests pass
- [x] `cargo clippy` — no warnings
- [x] reqwest automatically advertises `br`, `zstd`, `deflate` in `Accept-Encoding`

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Larger binary (additional decompression libraries) | High | Low | Trivial increase; brotli/zstd are already common Rust dependencies |
| Upstream returns garbage with unsupported content-encoding | Low | Low | Reqwest validates decompression; errors surface as HTTP errors to client |

---

## Archive Information

**Archived:** 2026-06-22
**Duration:** < 1 day
**Outcome:** Successfully implemented

### Files Modified
- `Cargo.toml` — added `brotli`, `zstd`, `deflate` to reqwest features

### Specs Updated
- `openspec/specs/deployment.md` — added HTTP Client Compression requirement
