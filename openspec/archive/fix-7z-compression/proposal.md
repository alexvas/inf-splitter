# Proposal: Implement 7z and Bz2 compression for rotated diagnostic files

**Change ID:** `fix-7z-compression`
**Created:** 2026-06-21
**Status:** Complete

---

## Problem Statement

Despite configuring `compression = "7z"` in `[diagnostics]`, rotated dump files remain uncompressed. The disk fills up with 50 MiB `.ndjson` files when they should be compressed to `.ndjson.7z`.

Root cause: only `Compression::Zip` is implemented in `compress_file()`. The `SevenZ` and `Bz2` variants are stubs that log `"compression not yet implemented, leaving uncompressed"` and do nothing — they don't even remove the original file.

## Proposed Solution

Implement all `Compression` variants using pure-Rust crates (`sevenz-rust`, `bzip2`), following the existing `Zip` pattern. Extracted a shared `compress_with_output` helper to eliminate duplication across the three backends.

## Scope

### In Scope
- Add `sevenz-rust` and `bzip2` dependencies
- Implement `Compression::SevenZ` arm using `SevenZWriter::push_source_path`
- Implement `Compression::Bz2` arm using `BzEncoder`
- Extract `compress_with_output` helper for create-output/compress/cleanup
- Remove original file after successful compression
- Handle errors consistently (log + cleanup)

### Out of Scope
- Changing the `Compression` enum or config parsing

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `Cargo.toml` | Yes | Add `sevenz-rust`, `bzip2` |
| `diagnostics.rs` | Yes | Implement `SevenZ` + `Bz2` arms, extract helper |
| Tests | Yes | `diagnostics_rotation_compresses_with_7z`, `diagnostics_rotation_compresses_with_bz2` |

## Architecture Considerations

Pure-Rust crates — no system binaries required. Compression runs in `spawn_blocking`, doesn't block the writer loop. The `compress_with_output` helper eliminates ~60 lines of duplicated open/create/compress/cleanup across the three backends.

## Success Criteria

- [x] Rotated `.ndjson` file is compressed to `.ndjson.7z` when `compression = "7z"`
- [x] Rotated `.ndjson` file is compressed to `.ndjson.bz2` when `compression = "bz2"`
- [x] Original `.ndjson` file is removed after successful compression
- [x] On compression failure, original file is preserved and error is logged
- [x] `cargo fmt --check`, `cargo clippy`, `cargo test` pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| `sevenz-rust` API changes | Low | Low | Pin to a compatible version range |
| Large file compression takes time | Medium | Low | Runs in `spawn_blocking`, doesn't block writer loop |

---

## Archive Information

**Archived:** 2026-06-21
**Duration:** 1 day
**Outcome:** Successfully implemented

### Files Modified
- `Cargo.toml` — added `sevenz-rust`, `bzip2`
- `src/diagnostics.rs` — implemented `SevenZ` + `Bz2` arms, extracted `compress_with_output` helper
- `tests/protocol_conversion.rs` — 7z and Bz2 compression tests

### Specs Updated
- `openspec/specs/diagnostics.md` — added 7z, Bz2, and failure-preserves-original scenarios
