# Delta: Diagnostics

**Change ID:** `reqwest-gzip-decompression`
**Affects:** `src/diagnostics.rs`, `Cargo.toml` (reqwest features)

---

## MODIFIED

### Requirement: Stats Event Format

The `error` field in `StatsEvent` contains the upstream error response body as a UTF-8 string. The proxy uses reqwest with `gzip` compression support enabled — upstream responses with `Content-Encoding: gzip` are automatically decompressed, so the `error` field always contains human-readable text (not raw gzip bytes).

#### Scenario: Upstream gzip error decoded
- GIVEN upstream (e.g. Gemini API) returns a gzip-compressed 401 error
- WHEN the stats event is recorded
- THEN `error` contains the decoded JSON error body, not raw binary
