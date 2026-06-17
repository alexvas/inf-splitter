# Delta: Configuration

**Change ID:** `configurable-drop-fields`
**Affects:** `config.rs`, `lib.rs`

---

## ADDED

### Requirement: Per-Section drop_fields

Each provider section can specify `drop_fields` in one of two forms:

**Flat list** — same fields for every model in the section:
```toml
drop_fields = ["thinking", "stream_options"]
```

**Per-model map** — `"all"` provides base fields, model-specific keys add extra fields:
```toml
[deepseek.drop_fields]
all = ["thinking"]
"deepseek-v4-pro" = ["context_management"]
```

At request time, `all` fields and the matched model's fields are merged (additive — model-specific keys never replace `all`). `"all"` is a reserved key; it does not match any model literally.

#### Scenario: Flat list applies to all models
- GIVEN section has `drop_fields = ["thinking"]` and models `["a", "b"]`
- WHEN a request arrives for model `"a"` or `"b"`
- THEN `thinking` is dropped from the outgoing body

#### Scenario: Per-model merge with "all"
- GIVEN `[s.drop_fields]` has `all = ["thinking"]` and `"deepseek-v4-pro" = ["context_management"]`
- WHEN a request arrives for model `"deepseek-v4-pro"`
- THEN both `thinking` and `context_management` are dropped
- WHEN a request arrives for model `"deepseek-v4-pro[1m]"` (not listed in drop_fields)
- THEN only `thinking` is dropped (from `"all"`)

#### Scenario: Model-specific without "all"
- GIVEN `[s.drop_fields]` has `"model-x" = ["foo"]` and no `all` key
- WHEN a request arrives for model `"model-x"`
- THEN `foo` is dropped
- WHEN a request arrives for model `"model-y"`
- THEN nothing is dropped

#### Scenario: drop_fields absent is a no-op
- GIVEN section has no `drop_fields` key
- WHEN a request body is forwarded
- THEN all client fields are passed through unchanged

#### Scenario: Dropping non-existent field is silent
- GIVEN `drop_fields = ["nonexistent"]`
- WHEN a request body without `nonexistent` is processed
- THEN the body is forwarded unchanged, no error raised

---

## MODIFIED

### Requirement: Provider Sections

Updated TOML schema — each provider section now accepts an optional `drop_fields` key in one of two forms:

| Form | TOML | Type |
|------|------|------|
| Flat list | `drop_fields = ["a", "b"]` | `[String]` |
| Per-model map | `[s.drop_fields]` with `all = [...]` and `"model" = [...]` | table of `[String]` |

When using the per-model form, `all` is a reserved key that provides the base set; model-specific keys add to it.

### Requirement: Token Limit Injection

`apply_token_caps_to_value()` is now followed by `drop_fields_from_value()` in the request processing pipeline. Both operate on the same `serde_json::Value` before serialization.
