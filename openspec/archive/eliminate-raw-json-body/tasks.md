# Implementation Tasks: Eliminate raw JSON body from interactions pipeline

**Change ID:** `eliminate-raw-json-body`

---

## Phase 1: `build_request_body` — typed params, typed return

- [x] 1.1 Replace `body: &Value, system_fn` with `stream: bool, temperature: Option<f64>, ingress_max_tokens: Option<u32>, system_instruction: Option<String>`
- [x] 1.2 Return `CreateModelInteractionParams` instead of `serde_json::Value`
- [x] 1.3 Update unit tests

**Quality Gate:** PASSED

---

## Phase 2: Typed message/system extractors

- [x] 2.1 `build_interactions_request_anthropic` takes `messages: &[Value]` instead of `body: &Value`, plus typed scalars
- [x] 2.2 `build_interactions_request_openai` takes `messages: &[Value]` instead of `body: &Value`, plus typed scalars
- [x] 2.3 `extract_anthropic_system` kept as pub fn for handler use
- [x] 2.4 `extract_openai_system` internal, operates on `&[Value]` messages slice

**Quality Gate:** PASSED

---

## Phase 3: Typed control-message cleaning

- [x] 3.1 Handlers pass `control_result.cleaned_messages` directly (as `Vec<Value>`) instead of cloning body + mutating JSON
- [x] 3.2 No more `request_body_val` — cleaned messages passed directly to build functions

**Quality Gate:** PASSED

---

## Phase 4: Callers in `interactions_handler.rs`

- [x] 4.1 Typed scalars extracted from `body_val` at boundary (`stream`, `temperature`, `ingress_max_tokens`, `system`)
- [x] 4.2 Split-path reads `params.input` (as `InteractionsInput::ContentList`), `params.system_instruction`, `params.previous_interaction_id`
- [x] 4.3 `handle_split_send` takes `&CreateModelInteractionParams` and `&[Content]` instead of `&Value` and `&[Value]`
- [x] 4.4 Serialize `CreateModelInteractionParams` to bytes at HTTP send site

**Quality Gate:** PASSED

---

## Phase 5: Documentation

- [x] 5.1 Add CLAUDE.md rule: "Parse ingress JSON into typed structs at the protocol boundary. Pass typed values down the call stack."
- [x] 5.2 Full verification: `cargo fmt --check && cargo clippy --locked -- -D warnings && cargo test --locked` — ALL PASS

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Ready for `/openspec-archive`
