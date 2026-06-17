# Delta: Protocol Conversion

**Change ID:** `configurable-drop-fields`
**Affects:** `src/openai.rs`, `src/anthropic.rs`, `src/lib.rs`

---

## ADDED

### Requirement: drop_fields_from_value Helper

`lib.rs` gains a new function that removes specified top-level keys from a `serde_json::Value`:

```rust
pub(crate) fn drop_fields_from_value(value: &mut Value, fields: &HashSet<String>) {
    if fields.is_empty() { return; }
    if let Some(obj) = value.as_object_mut() {
        for field in fields {
            obj.remove(field.as_str());
        }
    }
}
```

The set of fields to drop is resolved at request time from the `RouteTarget` by merging `all` and the matched model's entry.

#### Scenario: Multiple fields dropped
- GIVEN fields set `{"a", "b"}` and body `{"a":1,"b":2,"c":3}`
- WHEN `drop_fields_from_value` is called
- THEN the value becomes `{"c":3}`

#### Scenario: Empty set is a no-op
- GIVEN an empty set and any body
- WHEN `drop_fields_from_value` is called
- THEN the body is unchanged (zero overhead for sections without drop_fields)

### Requirement: Field Drop on All Routing Paths

The `drop_fields` are applied on all four request paths:

| Path | Insertion Point |
|------|----------------|
| OpenAI passthrough | After `apply_token_caps_to_value`, before `serde_json::to_vec` (both streaming and non-streaming) |
| Anthropic passthrough | Body parsed as `Value`, drops applied, then parsed into `MessageCreateRequest` |
| Anthropic→OpenAI conversion | On the `Value` after translation, before serialization upstream |
| OpenAI→Anthropic conversion | On the ingress `Value` before translation (since translation operates on typed structs) |

#### Scenario: Drop on OpenAI passthrough
- GIVEN `drop_fields = ["user"]` on an OpenAI-passthrough section
- WHEN a request with `{"model":"x","user":"abc","messages":[...]}` is processed
- THEN the upstream receives `{"model":"x","messages":[...]}` without the `user` field

#### Scenario: Drop on Anthropic passthrough
- GIVEN `drop_fields = ["metadata"]` on an Anthropic-passthrough section
- WHEN a request with `{"model":"x","metadata":{...},"max_tokens":100,"messages":[...]}` is processed
- THEN the upstream receives the body without the `metadata` field

#### Scenario: Drop on conversion path
- GIVEN `drop_fields = ["logprobs"]` and an OpenAI→Anthropic conversion section
- WHEN an OpenAI ingress request includes `"logprobs": true`
- THEN the translated Anthropic request does not contain `logprobs` in its upstream JSON
