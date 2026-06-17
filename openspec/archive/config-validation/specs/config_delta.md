# Delta: Configuration

**Change ID:** `config-validation`
**Affects:** `src/config.rs`

---

## ADDED

### Requirement: Model Name Validation in Lists

When `models` is a list, each entry is validated: trimmed whitespace, rejected if empty. Previously only the single-string form checked for emptiness; list entries with empty or whitespace-only strings silently registered blank routes.

#### Scenario: Empty string in model list
- GIVEN `models = ["valid", ""]`
- WHEN config is loaded
- THEN startup fails with `ConfigError::Provider { name, message: "model name must not be empty" }`

#### Scenario: Whitespace-only in model list
- GIVEN `models = ["valid", "  "]`
- WHEN config is loaded
- THEN startup fails with the same error

---

### Requirement: drop_fields Per-Model Key Validation

When `drop_fields` uses the per-model form, each model-specific key (anything except `"all"`) must match a model in the section's `models` list. Unknown keys are a configuration error.

#### Scenario: drop_fields references unknown model
- GIVEN `models = ["known-model"]` and `[s.drop_fields] "unknown-model" = ["field"]`
- WHEN config is loaded
- THEN startup fails with `ConfigError::UnknownDropModel { section: "s", model: "unknown-model" }`

#### Scenario: drop_fields "all" key is always valid
- GIVEN `models = ["known-model"]` and `[s.drop_fields] all = ["field"]`
- WHEN config is loaded
- THEN no error (all models in section get the base fields)

---

## MODIFIED

### Requirement: `[defaults]` Section

`DefaultConfig` now has `#[serde(deny_unknown_fields)]`. Unknown keys in `[defaults]` cause a TOML parse error naming the unknown field.

#### Scenario: Valid defaults
- GIVEN `[defaults]` with only `max_tokens = 4096`
- WHEN config is loaded
- THEN parsed successfully

#### Scenario: Unknown field in defaults
- GIVEN `[defaults]` with `endpoint_openai = "http://x"`
- WHEN config is loaded
- THEN TOML parse fails with "unknown field `endpoint_openai`"
