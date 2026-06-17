# Implementation Tasks: Configurable Drop of Request Parameters

**Change ID:** `configurable-drop-fields`

Each step is a RED→GREEN pair: write the test first, watch it fail, then implement.

---

## Step 1: Config parsing — flat list

- [x] 1.1 **RED** — Unit test: `drop_fields = ["thinking", "stream_options"]` parses into `DropFields::All`
- [x] 1.2 **GREEN** — `DropFields` enum + `Deserialize` (flat list variant) + wire through `ProviderSection` / `RouteTarget`

---

## Step 2: Config parsing — per-model map

- [x] 2.1 **RED** — Unit tests: `[s.drop_fields] all = [...]` + per-model keys parse into `DropFields::PerModel`
- [x] 2.2 **RED** — Unit test: `drop_fields` absent → `DropFields::All(HashSet::new())` (no-op)
- [x] 2.3 **GREEN** — `Deserialize` for per-model table form + `DropFields::for_model()` merge logic

---

## Step 3: drop_fields_from_value helper

- [x] 3.1 **RED** — Unit test: `drop_fields_from_value` removes matching keys, leaves rest, no-op on empty set, no-op on non-existent key
- [x] 3.2 **GREEN** — `drop_fields_from_value(value: &mut Value, fields: &HashSet<String>)` in `lib.rs`

---

## Step 4: OpenAI passthrough (non-streaming + streaming)

- [x] 4.1 **RED** — Integration test: flat `drop_fields = ["user"]`, passthrough request, verify upstream body lacks `"user"`
- [x] 4.2 **GREEN** — Call `drop_fields_from_value` in `OpenAiHandler` passthrough (after `apply_token_caps_to_value`, before serialize) — both non-streaming and streaming

---

## Step 5: Anthropic passthrough

- [x] 5.1 **RED** — Integration test: flat `drop_fields`, Anthropic passthrough request, verify upstream body lacks the field
- [x] 5.2 **GREEN** — Parse ingress body as `Value`, apply drops, serialize back, then deserialize into `MessageCreateRequest`

---

## Step 6: Conversion paths

- [x] 6.1 **RED** — Integration test: Anthropic→OpenAI conversion with `drop_fields`, verify translated upstream body
- [x] 6.2 **RED** — Integration test: OpenAI→Anthropic conversion with `drop_fields`, verify translated upstream body
- [x] 6.3 **GREEN** — Call `drop_fields_from_value` in both conversion paths (on `Value` after translation, before serialize; also on ingress `Value` before `ChatCompletionRequest` deserialization)

---

## Step 7: Per-model granularity

- [x] 7.1 **RED** — Integration test: per-model `drop_fields` with `all` + model-specific key; request for matched model drops both, other model drops only `all`
- [x] 7.2 **GREEN** — Resolve drop set at request time via `DropFields::for_model(&model)` — merge `all` ∪ model-specific

---

## Step 8: No-op & absent

- [x] 8.1 **RED** — Integration test: section without `drop_fields` passes all fields unchanged
- [x] 8.2 **RED** — Integration test: `drop_fields = []` passes all fields unchanged
- [x] 8.3 **GREEN** — Verify existing implementation (already handled by empty `HashSet` fast-path in `drop_fields_from_value`); fix if red

---

## Step 9: Documentation & Polish

- [x] 9.1 Add `drop_fields` to example config with both forms commented
- [x] 9.2 Update README.md (Russian)
- [x] 9.3 Update README.en.md (English)
- [x] 9.4 Update README.zh.md (Chinese)
- [x] 9.5 `cargo fmt --check` + `cargo clippy` clean

---

## Completion Checklist

- [x] All red→green steps pass
- [x] `cargo test --locked` — all tests green (87 unit + 8 e2e + 44 integration = 139)
- [x] `cargo fmt --check` — clean
- [x] `cargo clippy --locked -- -D warnings` — clean
- [x] READMEs synced (all three languages, 24 headings each)
- [x] Ready for `/openspec-archive`
