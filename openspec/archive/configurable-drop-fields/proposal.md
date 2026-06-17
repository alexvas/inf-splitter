# Proposal: Configurable Drop of Request Parameters

**Change ID:** `configurable-drop-fields`
**Created:** 2026-06-17
**Status:** Implementation Complete
**Completed:** 2026-06-17
**Archived:** 2026-06-17

---

## Problem Statement

Some upstream providers reject or misbehave on certain JSON fields that clients send. Currently the proxy has two hardcoded workarounds:

- `stream_options` is always stripped in the Anthropic→OpenAI conversion path (`openai.rs:268,457`)
- `thinking.type = "adaptive"` is stripped before Anthropic→OpenAI translation (`openai.rs:801`)

Adding or changing which fields to drop requires a code change. Operators need a way to configure field removal per provider section without modifying the proxy binary.

## Proposed Solution

Add an optional per-section TOML key `drop_fields` — either a flat list (applies to all models in the section) or a `[section.drop_fields]` sub-table keyed by model name with `"all"` as a special catch-all key.

**Flat list — same fields for every model in the section:**

```toml
[deepseek]
endpoint_anthropic = "https://api.deepseek.com/anthropic"
models = ["deepseek-v4-pro", "deepseek-v4-pro[1m]"]
api_key = "${DEEPSEEK_API_KEY}"
drop_fields = ["thinking", "stream_options"]
```

**Per-model — `"all"` is the base set, model-specific keys are additive:**

```toml
[deepseek]
endpoint_anthropic = "https://api.deepseek.com/anthropic"
models = ["deepseek-v4-pro", "deepseek-v4-pro[1m]"]
api_key = "${DEEPSEEK_API_KEY}"

[deepseek.drop_fields]
all = ["thinking"]                             # applies to all models
"deepseek-v4-pro" = ["context_management"]     # only for this model
# deepseek-v4-pro[1m] — only "thinking" dropped (from "all")
```

At request time:
1. Look up the resolved model name
2. Merge `all` fields (if any) + model-specific fields (if any) into a single set
3. Remove those keys from the parsed JSON body

This happens after model extraction and token limit injection, before serialization upstream. Covers both passthrough and conversion paths.

## Scope

### In Scope
- New `drop_fields` per-section TOML key — flat list `["a","b"]` or per-model map `{all = [...], "model-x" = [...]}`
- `"all"` as a special keyword providing base fields for every model in the section
- Model-specific keys are additive (merged with `"all"`, not replacing)
- Removal of specified top-level JSON keys from outgoing request bodies
- Works on all four routing paths: passthrough (OpenAI, Anthropic) and conversion (both directions)

### Out of Scope
- Nested field removal (e.g., `"messages[0].content"`) — top-level keys only
- Field removal from response bodies
- Regex/wildcard field matching (model names are exact match, field names are exact match)
- Per-model keys **replacing** `"all"` — they are always merged (additive)
- Removing existing hardcoded `stream_options` stripping (it stays as-is, can be supplemented by config)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| Config model | Yes | Add `drop_fields` to `ProviderConfigRaw`, `ProviderSection`, `RouteTarget` |
| Config tests | Yes | Test parsing of `drop_fields` list |
| OpenAI handler | Yes | Call `drop_fields` on parsed JSON body in passthrough and conversion paths |
| Anthropic handler | Yes | Call `drop_fields` on parsed JSON body in passthrough and conversion paths |
| lib.rs | Yes | Add `drop_fields_from_value()` helper alongside `apply_token_caps_to_value()` |
| READMEs | Yes | Document new key (all three languages) |
| Example config | Yes | Add commented `drop_fields` example |

## Architecture Considerations

The natural insertion point is alongside `apply_token_caps_to_value()` — the body is already parsed as `serde_json::Value` for token cap injection. We add a `drop_fields_from_value()` call that takes a `&HashSet<String>` and removes matching keys via `Value::as_object_mut().and_then(|obj| obj.remove(field))`.

Since the body is already in `Value` form at this point, field removal costs essentially zero (just `BTreeMap::remove` calls).

**Config representation** — `RouteTarget` stores a `DropFields` enum:

```rust
enum DropFields {
    All(HashSet<String>),                        // flat list
    PerModel { all: HashSet<String>, by_model: HashMap<String, HashSet<String>> },
}
```

At request time, `DropFields::for_model(model: &str) -> &HashSet<String>` returns the merged set: `all ∪ by_model[model]`. The result is computed once when the route is resolved and cached in the route target (or computed lazily at request time — the sets are small, merging is cheap).

**Anthropic passthrough path** — the ingress body is currently parsed directly into a typed `MessageCreateRequest` struct. For this path, we parse the raw bytes as `Value` first, apply drops, serialize back to bytes, then parse into `MessageCreateRequest`. This adds one extra deserialization pass but keeps the implementation clean (no need to drop fields from the typed struct, which doesn't support arbitrary keys anyway).

## Success Criteria

- [ ] `drop_fields = ["foo", "bar"]` in TOML removes `foo` and `bar` from outgoing request body
- [ ] `drop_fields` absent or empty → no fields removed (backward compatible)
- [ ] Fields are dropped on all four routing paths (OpenAI passthrough, Anthropic passthrough, Anthropic→OpenAI conversion, OpenAI→Anthropic conversion)
- [ ] `cargo fmt --check`, `cargo clippy`, `cargo test` all pass
- [ ] READMEs updated in all three languages

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Dropping a required field breaks upstream | Medium | Medium | Document that operators should test with their upstream; the feature is opt-in per section |
| Field name typo silently does nothing | Low | Low | Accept — same as any config typo; no validation of field existence is practical |
