# Delta: Configuration

**Change ID:** `replace-body-too-large-hint-with-error-translation`
**Affects:** `src/config.rs`, global settings in TOML

---

## ADDED

### Requirement: Error Translation Rules

The optional `[[error_translation]]` array in the top-level TOML config defines rules for translating upstream error response bodies before returning them to the client. Each rule has:

| Field | Required | Description |
|-------|----------|-------------|
| `status` | Yes | HTTP status code (u16) to match |
| `ingress` | No | Substring to match in the upstream error body; absent/empty = match any body |
| `egress` | Yes | Replacement body string (replaces the entire upstream body) |

Rules are evaluated in definition order; first match wins. If no rule matches, the upstream body passes through unchanged. An empty or absent `error_translation` disables translation entirely.

```toml
[[error_translation]]
status = 413
ingress = "some vague message substring"
egress = "body too large"

[[error_translation]]
status = 502
egress = "BODY TOO LARGE"
```

#### Scenario: Status+substring match
- GIVEN `status = 413`, `ingress = "vague"`, `egress = "translated"`
- WHEN upstream returns 413 with body `"a vague error"`
- THEN the body is replaced with `"translated"`

#### Scenario: Status-only match
- GIVEN `status = 502`, no `ingress`, `egress = "replaced"`
- WHEN upstream returns 502 with body `"any error text"`
- THEN the body is replaced with `"replaced"`

#### Scenario: No match — pass-through
- GIVEN a rule matches status 413 only
- WHEN upstream returns 500 with body `"server error"`
- THEN the body passes through unchanged

#### Scenario: First-match ordering
- GIVEN two rules for the same status 413
- WHEN upstream returns 413
- THEN the first matching rule's `egress` is used

#### Scenario: Substring mismatch
- GIVEN `status = 413`, `ingress = "vague"`, `egress = "translated"`
- WHEN upstream returns 413 with body `"different error"`
- THEN the body passes through unchanged (no substring match)

#### Scenario: Empty rules — no translation
- GIVEN no `[[error_translation]]` entries in config
- WHEN any upstream error occurs
- THEN all bodies pass through unchanged

---

## MODIFIED

### Requirement: Global Settings

Top-level config keys:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `listen_host` | string | `127.0.0.1` | Bind address |
| `listen_port` | u16 | `3000` | Bind port |
| `upstream_timeout` | duration | `5m` | HTTP timeout for upstream calls |
| `max_request_body` | byte size | `2m` | Max incoming body size |
| `[[error_translation]]` | array of tables | (none) | Optional upstream error body translation rules |

(Removed `body_too_large_hint_statuses`.)

---

## REMOVED

### Requirement: body_too_large_hint_statuses

The `body_too_large_hint_statuses` setting (list of HTTP status codes, default `[413]`) and its associated `append_size_hint()` function are removed. The error hint mechanism is replaced by the more general `[[error_translation]]` rules.
