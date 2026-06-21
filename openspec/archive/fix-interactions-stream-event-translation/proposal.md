# Proposal: Fix Interactions Stream Event Translation

**Change ID:** `fix-interactions-stream-event-translation`
**Created:** 2026-06-21
**Status:** Implementation Complete
**Completed:** 2026-06-21

---

## Problem Statement

The proxy's Interactions handler fails to translate upstream SSE events, causing the client to receive an **empty stream** and retry the same request repeatedly. A dump at `/tmp/dump-gemini.ndjson` shows 4 identical ingress requests (request IDs 0–3) with only a single upstream response (line 3). The proxy received a valid SSE response from Gemini, but `translate_stream_event` returned `None` for **every** event, so the response stream relayed to the client was empty.

### Root cause 1: generated serde tag values are wrong (build.rs)
`InteractionSseEvent` — the discriminated union for SSE events — is generated with wrong `#[serde(rename)]` values. The real API returns event_type values in **lowercase dot notation**: `"interaction.created"`, `"step.start"`, `"step.delta"`, `"step.stop"`, `"interaction.completed"`. But the generated code has:
```rust
#[serde(rename = "interaction_created_event")]  // wrong: should be "interaction.created"
InteractionCreatedEvent(InteractionCreatedEvent),
```

This happens because `InteractionSseEvent`'s discriminator in the OpenAPI schema has **no `mapping`** — only `propertyName: "event_type"`. The individual event schemas **do** have the correct tag via `const` on their `event_type` property:
```json
{ "properties": { "event_type": { "const": "interaction.status_update" } } }
```

But `build.rs` never reads those `const` values. When `mapping` is absent, it falls back to `derive_tag_from_variant()` which mechanically transforms the struct name:
```
derive_tag_from_variant("InteractionCreatedEvent", "InteractionSseEvent")
→ strip_suffix("InteractionSseEvent") fails (no match)
→ to_snake_case("InteractionCreatedEvent") = "interaction_created_event"  // WRONG
```

### Root cause 2: translate_stream_event uses hardcoded wrong strings
`translate_stream_event` does its own manual event_type matching with `"INTERACTION_CREATED"`, `"CONTENT_DELTA"`, `"INTERACTION_COMPLETED"` (SCREAMING_SNAKE_CASE) instead of using the generated `InteractionSseEvent` enum for deserialization. Even if the generation were fixed, the hardcoded strings are a second point of failure.

### Root cause 3: step.* events not handled
The new Gemini Interactions streaming protocol uses `step.start`, `step.delta`, `step.stop` events with a `Step` object carrying a `type` field (`"thought"`, `"model_output"`, etc.). The old `content.delta` / `ContentDelta` events are no longer emitted. The handler has no code to process step-based events.

### Root cause 4: Struct mismatch
`StepDelta` has `delta: StepDeltaData` (an enum including `TextDelta`, `ThoughtSignatureDelta`, etc.), while `ContentDelta` has `delta: ContentDeltaData` (only `TextDelta`). The old deserialization into `ContentDelta` can't capture `ThoughtSignatureDelta` events.

### Effect
The client sends a request, the proxy forwards it upstream, the upstream responds correctly, but the proxy streams **zero bytes** back to the client. The client interprets this as a failure and retries, creating an infinite loop until the client gives up.

## Proposed Solution

Two-layer fix — generation layer + handler layer:

### Layer 1: Fix build.rs to generate correct serde tag values

In `resolve_schema`, when processing a oneOf discriminator **without** an explicit `mapping`, look at each variant's resolved schema for a `const` value on the discriminator property. If found, use that `const` as the serde tag instead of the fallback `derive_tag_from_variant()`.

Example: for `InteractionSseEvent` variant `InteractionStatusUpdate`:
1. Schema `InteractionStatusUpdate` has `properties.event_type.const = "interaction.status_update"`
2. Use `"interaction.status_update"` as the `#[serde(rename)]` value
3. Generated: `#[serde(rename = "interaction.status_update")] InteractionStatusUpdate(...)`

This fixes the generated `InteractionSseEvent` enum so it correctly deserializes real API responses with `serde_json::from_str::<InteractionSseEvent>(data)`.

### Layer 2: Rewrite translate_stream_event to use InteractionSseEvent

Replace the manual `peek.get("event_type")?.as_str()` + string match with typed deserialization. This is type-safe — the compiler guarantees all variants are handled, no string mismatch is possible.

## Scope

### In Scope
- Fix `build.rs` to read `const` values from variant schemas for oneOf discriminator tags (when no explicit mapping exists)
- Regenerate `interactions_types.rs` with correct `serde(rename)` values on `InteractionSseEvent`
- Rewrite `translate_stream_event` to use `InteractionSseEvent` enum for type-safe deserialization
- Add `StepStart`, `StepDelta`, `StepStop` event handlers
- Map `ThoughtSignatureDelta` to `signature_delta` SSE events
- Update unit tests to use correct event_type values and step-based events

### Out of Scope

- Non-streaming response translation (already works)
- Adding a formal `mapping` to the OpenAPI schema (build.rs infers from `const` instead)

**Handling policy for unsupported-but-valid events** (three categories):

| Category | Example | Behavior |
|----------|---------|----------|
| **Malformed data** | Invalid JSON, unknown `event_type` | `tracing::info!` with the raw data prefix, then drop |
| **Valid, no client impact** | `interaction.status_update` | Skip with a code comment explaining why it's safe |
| **Valid, not yet implemented** | Unhandled delta types | Log via `tracing::warn!` with event type and reason |

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `build.rs` | Yes | Extract `const` values from variant schemas for discriminator tags when mapping is absent |
| `interactions_types.rs` | Regenerated | `InteractionSseEvent` variants will have correct `serde(rename)` values |
| `interactions_handler.rs:translate_stream_event` | Yes | Replace manual string matching with `InteractionSseEvent` deserialization; add StepStart/StepDelta/StepStop arms |
| Tests | Yes | Update existing SSE event tests; add step.* tests |

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| anyllm_translate lacks `signature_delta` StreamEvent variant | Medium | Medium | Serialize via `serde_json::from_value(serde_json::json!({...}))` as already done for other events |
| Breaking old Gemini API versions that use `content.delta` | Low | Low | Keep old ContentDelta handler as a fallback match arm |

---

## Archive Information

**Archived:** 2026-06-21
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `build.rs` — added `try_const_tag()` helper; wired into `resolve_schema`
- `src/interactions_handler.rs` — rewrote `translate_stream_event` to use `InteractionSseEvent`; added step.*/ErrorEvent handling; updated interaction_id tracking
- `src/interactions_types.rs` — updated test expectation for correct event_type value
- `tests/e2e.rs` — updated 2 streaming e2e tests to use new event types

### Specs Updated
- `openspec/specs/protocol-conversion.md` — updated Interactions Streaming Events; added build.rs discriminator tag inference, typed event dispatch, JSON roundtrip documentation, handling policy for unsupported events
