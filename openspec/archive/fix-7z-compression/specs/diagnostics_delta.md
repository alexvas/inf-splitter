# Delta: Diagnostics — 7z and Bz2 Compression

**Change ID:** `fix-7z-compression`
**Affects:** `src/diagnostics.rs`, `Cargo.toml`

---

## MODIFIED

### Requirement: File Rotation — Compression

All `Compression` variants (`zip`, `7z`, `bz2`) are now implemented. After a file is rotated, it is compressed in a background blocking thread. On success, the original uncompressed file is removed. On failure, the original is preserved and an error is logged.

#### Scenario: 7z compression after rotation
- GIVEN `compression = "7z"` and `max_file_size` is set
- WHEN a dump file is rotated
- THEN the rotated file is compressed to `.ndjson.7z`
- AND the original `.ndjson` file is removed

#### Scenario: Bz2 compression after rotation
- GIVEN `compression = "bz2"` and `max_file_size` is set
- WHEN a dump file is rotated
- THEN the rotated file is compressed to `.ndjson.bz2`
- AND the original `.ndjson` file is removed

#### Scenario: Compression failure preserves original
- GIVEN any compression is configured
- AND the compression fails (e.g., disk full)
- WHEN compression is attempted
- THEN the original `.ndjson` file is preserved
- AND an error is logged

#### Scenario: Zip compression unchanged
- GIVEN `compression = "zip"`
- WHEN a dump file is rotated
- THEN behavior is unchanged (`.ndjson.zip` produced, original removed)
