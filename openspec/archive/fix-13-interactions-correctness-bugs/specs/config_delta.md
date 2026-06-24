# Delta: Configuration

**Change ID:** `fix-13-interactions-correctness-bugs`
**Affects:** `src/config.rs`

---

## ADDED

(None)

## MODIFIED

### Requirement: Secret Resolution Validates Header Safety

Secret resolution (`${VAR}` substitution) for `api_key` values must validate that the resolved value is a legal HTTP header value. The check uses `HeaderValue::from_str` — if it fails, the error is surfaced at startup with the section name and a description of the problem.

#### Scenario: Secret with newlines rejected
- GIVEN `api_key = "${KEY}"` where `secrets/KEY` contains `"abc\ndef"`
- WHEN config is loaded
- THEN `ConfigError::InvalidApiKey` is returned

## REMOVED

(None)
