# Delta: Project

**Change ID:** `reqwest-gzip-decompression`
**Affects:** `Cargo.toml`, `src/main.rs`

---

## MODIFIED

### Requirement: Tech Stack — HTTP Client

reqwest is configured with `default-features = false` and explicit feature list. Added `"gzip"` for automatic upstream response decompression.

Updated features:
```
reqwest = { version = "0.12", default-features = false, features = [
    "json", "stream", "rustls-tls-native-roots", "system-proxy", "socks", "gzip"
] }
```

#### Scenario: gzip decompression enabled
- GIVEN an upstream returns `Content-Encoding: gzip`
- WHEN reqwest reads the response body via `.text()`
- THEN the body is automatically decompressed to UTF-8

---

## ADDED

### Requirement: Version in Startup Log

The startup `info!` log includes `version` from `CARGO_PKG_VERSION`:

```
INFO inf_splitter: starting inf-splitter version="1.6.4" listen=127.0.0.1:3000 ...
```

#### Scenario: Version logged on startup
- GIVEN Cargo.toml has `version = "1.6.4"`
- WHEN inf-splitter starts
- THEN `version="1.6.4"` appears in the startup log line
