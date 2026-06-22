# Delta: Deployment & Tech Stack

**Change ID:** `enable-http-compression`
**Affects:** `Cargo.toml`, reqwest HTTP client behavior

---

## MODIFIED

### Requirement: HTTP Client Compression

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

#### Scenario: Upstream returns brotli-compressed response
- GIVEN an upstream returns `Content-Encoding: br`
- WHEN the proxy receives the response
- THEN reqwest transparently decompresses it before the proxy processes the body

#### Scenario: Upstream returns zstd-compressed response
- GIVEN an upstream returns `Content-Encoding: zstd`
- WHEN the proxy receives the response
- THEN reqwest transparently decompresses it before the proxy processes the body

#### Scenario: Accept-Encoding advertisement
- GIVEN the proxy is built with all compression features
- WHEN an egress request is sent upstream
- THEN the `Accept-Encoding` header includes `gzip, br, zstd, deflate`

#### Scenario: Gzip fallback still works
- GIVEN an upstream only supports `gzip`
- WHEN the proxy sends a request
- THEN the upstream returns `Content-Encoding: gzip` and reqwest decompresses it as before
