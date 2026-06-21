# Delta: Interactions Request Construction Pipeline

**Change ID:** `eliminate-raw-json-body`
**Affects:** `src/interactions.rs`, `src/interactions_handler.rs`, `src/control.rs`, `CLAUDE.md`

---

## MODIFIED

### Requirement: Interactions request construction must use typed structs throughout

Previously `build_request_body()` accepted `body: &serde_json::Value` and returned `serde_json::Value`, with callers using `.get("field")` to read back fields. The pipeline now uses typed structs from ingress to egress:

- Ingress body parsed into `MessageCreateRequest` (Anthropic) or `ChatCompletionRequest` (OpenAI) at the protocol boundary
- `build_request_body()` takes typed scalars (`stream: bool`, `temperature: Option<f64>`, `ingress_max_tokens: Option<u32>`, `system_instruction: Option<String>`) and returns `CreateModelInteractionParams`
- Split-path reads struct fields directly (`params.input`, `params.system_instruction`, `params.previous_interaction_id`)
- Serialization to HTTP body bytes happens at the call site

#### Scenario: Anthropic ingress → Interactions request
- GIVEN a parsed `MessageCreateRequest` with model, messages, system, stream, temperature
- WHEN `build_interactions_request_anthropic` is called
- THEN messages are extracted via typed `extract_anthropic_messages(&request)`, system via `extract_anthropic_system(&request)`, and `build_request_body` receives only typed scalars
- AND the returned `CreateModelInteractionParams` can be serialized to JSON for the upstream HTTP call

#### Scenario: Split-path reads typed struct fields
- GIVEN a `CreateModelInteractionParams` returned by `build_request_body`
- WHEN the handler checks whether proxy_limit splitting is needed
- THEN `params.input`, `params.system_instruction`, `params.previous_interaction_id` are accessed as struct fields
- NOT via `.get("input")` on a `serde_json::Value`

---

## ADDED

### Requirement: Parse at ingress boundary, pass typed downstream

Raw `serde_json::Value` must not thread through functions when typed equivalents exist. Ingress JSON is parsed into typed structs (`MessageCreateRequest`, `ChatCompletionRequest`) at the protocol boundary. All downstream functions receive typed parameters.

#### Scenario: Control message cleaning on typed messages
- GIVEN a `Vec<InputMessage>` from a parsed `MessageCreateRequest`
- WHEN `scan_control_messages` processes the messages
- THEN cleaned messages are returned as `Vec<InputMessage>`, not as a mutated `serde_json::Value`

### Requirement: Typed message extractors

Protocol-specific extractors convert typed ingress message formats to `Vec<Content>` for the Interactions API:

- `extract_anthropic_messages(request: &MessageCreateRequest) -> Vec<Content>`
- `extract_openai_messages(request: &ChatCompletionRequest) -> Vec<Content>`
- `extract_anthropic_system(request: &MessageCreateRequest) -> Option<String>`
- `extract_openai_system(request: &ChatCompletionRequest) -> Option<String>`

---

## REMOVED

- `extract_anthropic_content(msg: &serde_json::Value) -> Option<Content>` — replaced by typed extractor
- `extract_openai_content(msg: &serde_json::Value) -> Option<Content>` — replaced by typed extractor
- `extract_anthropic_system(body: &serde_json::Value) -> Option<String>` — replaced by typed extractor
- `extract_openai_system(body: &serde_json::Value) -> Option<String>` — replaced by typed extractor
- `body: &serde_json::Value` parameter from `build_request_body` — replaced by typed scalars
- `.get("input")`, `.get("system_instruction")`, `.get("previous_interaction_id")` on returned Value — replaced by struct field access
