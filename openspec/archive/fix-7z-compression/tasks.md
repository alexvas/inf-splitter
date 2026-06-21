# Implementation Tasks: Implement 7z and Bz2 compression

**Change ID:** `fix-7z-compression`

---

## Phase 1: Dependencies

- [x] 1.1 Add `sevenz-rust` crate to `Cargo.toml`
- [x] 1.2 Add `bzip2` crate to `Cargo.toml`

**Quality Gate:**
- [x] `cargo check` passes

---

## Phase 2: Implementation

- [x] 2.1 Implement `Compression::SevenZ` arm in `compress_file()` using `SevenZWriter`
- [x] 2.2 Implement `Compression::Bz2` arm in `compress_file()` using `BzEncoder`
- [x] 2.3 Remove original file after successful compression
- [x] 2.4 Handle errors consistently with existing `Zip` arm

**Quality Gate:**
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes

---

## Phase 3: Tests

- [x] 3.1 Add test: 7z compression produces `.ndjson.7z` and removes original
- [x] 3.2 Add test: Bz2 compression produces `.ndjson.bz2` and removes original

**Quality Gate:**
- [x] `cargo test --locked` passes

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Ready for `/openspec-archive`
