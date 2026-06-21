# Delta: Protocol Conversion

**Change ID:** `fix-interactions-protocol-bugs`
**Affects:** `build.rs`, `src/interactions.rs`, `src/interactions_handler.rs`, `src/interactions_types.rs`

---

## MODIFIED

### Requirement: Interactions Request/Response Types

Rust types for the interactions protocol are generated at build time from `schemas/interactions.openapi.json` by `build.rs`. The generated code is included in `src/interactions_types.rs` via `include!`.

**Change:** The `Interaction` schema's `required` array is patched in `build.rs` before code generation to remove `created`, `updated`, and `steps` — the Gemini API does not consistently include these fields in SSE event payloads (specifically in `interaction.created` events where the interaction is still in-progress).

#### Scenario: Schema patching removes required fields
- GIVEN `schemas/interactions.openapi.json` has `Interaction.required: ["created", "id", "status", "steps", "updated"]`
- WHEN `build.rs` runs
- THEN the in-memory schema is patched so `Interaction.required` is `["id", "status"]`
- AND the generated `Interaction` struct has `created: Option<String>`, `updated: Option<String>`, `steps: Option<Vec<Step>>`

#### Scenario: interaction.created SSE event deserializes
- GIVEN SSE data `{"event_type":"interaction.created","interaction":{"id":"abc","status":"in_progress","model":"gemini-3.1-flash-lite"}}`
- WHEN `serde_json::from_str::<InteractionSseEvent>(data)` is called
- THEN it succeeds as `InteractionSseEvent::InteractionCreatedEvent`
- AND the inner `Interaction` has `created: None`, `steps: None`, `updated: None`

---

### Requirement: Anthropic → Interactions Translation

`InteractionsHandler` converts Anthropic ingress to `CreateModelInteractionParams`:
- `messages[]` → interactions `Content[]` via typed extractors
- `system` → `system_instruction`
- `max_tokens` → `generation_config.max_output_tokens`
- `previous_interaction_id` set from session state

**Change:** Also extract `tools` and `tool_choice` from the Anthropic ingress body and set them on `CreateModelInteractionParams.tools` and `CreateModelInteractionParams.tool_config`.

#### Scenario: Tools forwarded to interactions API
- GIVEN Anthropic ingress body with `"tools": [{"name": "get_weather", ...}]` and `"tool_choice": {"type": "auto"}`
- WHEN the interactions request is built
- THEN `CreateModelInteractionParams.tools` contains the tool definitions
- AND `CreateModelInteractionParams.tool_config` reflects the tool choice

---

### Requirement: OpenAI → Interactions Translation

OpenAI ingress → `CreateModelInteractionParams`:
- `messages[]` → interactions `Content[]` via typed extractors
- System message → `system_instruction`
- `max_tokens` → `generation_config.max_output_tokens`

**Change:** Also extract `tools` and `tool_choice` from the OpenAI ingress body and set them on `CreateModelInteractionParams.tools` and `CreateModelInteractionParams.tool_config`.

#### Scenario: OpenAI tools forwarded to interactions API
- GIVEN OpenAI ingress body with `"tools": [{"type": "function", "function": {"name": "search"}}]` and `"tool_choice": "auto"`
- WHEN the interactions request is built
- THEN `CreateModelInteractionParams.tools` contains the tool definitions
