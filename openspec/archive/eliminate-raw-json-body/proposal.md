# Proposal: Eliminate raw JSON body from interactions request construction

**Change ID:** `eliminate-raw-json-body`
**Created:** 2026-06-21
**Status:** Archived
**Archived:** 2026-06-21
**Duration:** <1 day

---

## Problem Statement

After the typed structs migration (`CreateModelInteractionParams`), the interactions pipeline still threads raw `serde_json::Value` through multiple functions:

- `build_request_body()` takes `body: &Value` for `stream`, `max_tokens` (fallback), `temperature`
- `build_interactions_request_anthropic`/`build_interactions_request_openai` take `body: &Value` for message and system extraction
- The returned `Value` from `build_request_body` is read back via `.get("input")`, `.get("system_instruction")`, `.get("previous_interaction_id")` in the split-path

This violates the principle: parse at ingress boundary, pass typed values downstream. It also creates a silent coupling — field names are string literals with no compiler verification.

## Proposed Solution

Three changes:

### 1. Replace `body: &Value` in `build_request_body()` with typed params

Remove the `body` parameter. Add three typed parameters extracted at the ingress boundary:
- `stream: bool`
- `temperature: Option<f64>`
- `ingress_max_tokens: Option<u32>` (fallback when `route.max_tokens` is None)

### 2. Typed message/system extractors

Replace `body: &Value` in `build_interactions_request_anthropic`/`build_interactions_request_openai`. Define extractors that work on the typed ingress structs:

- `extract_anthropic_messages(request: &MessageCreateRequest) -> Vec<Content>`
- `extract_openai_messages(request: &ChatCompletionRequest) -> Vec<Content>`
- `extract_anthropic_system(request: &MessageCreateRequest) -> Option<String>`
- `extract_openai_system(request: &ChatCompletionRequest) -> Option<String>`

The control-message cleaning (which currently mutates raw JSON) moves to operate on `Vec<InputMessage>` / `Vec<ChatMessage>` directly.

### 3. Replace returned `Value` with `CreateModelInteractionParams`

`build_request_body()` returns `CreateModelInteractionParams` directly instead of `serde_json::Value`. The callers in `interactions_handler.rs` access struct fields (`params.input`, `params.system_instruction`, `params.previous_interaction_id`) instead of `.get("input")` etc.

Serialization to bytes for the HTTP request happens at the call site.

### 4. CLAUDE.md rule

Add: "Parse ingress JSON into typed structs at the protocol boundary. Pass typed values down the call stack. Avoid threading raw `serde_json::Value` through functions when typed equivalents exist."

## Scope

### In Scope
- `build_request_body()`: eliminate `body: &Value`, return `CreateModelInteractionParams`
- `build_interactions_request_anthropic`/`build_interactions_request_openai`: typed message/system extraction
- Control-message cleaning: operate on typed message types
- `interactions_handler.rs`: split-path uses struct fields instead of `.get()`
- `CLAUDE.md`: new rule

### Out of Scope
- (none — the previously out-of-scope items are now included per user request)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions.rs` | Yes | `build_request_body` signature, new typed extractors |
| `interactions_handler.rs` | Yes | Callers pass typed params, split-path reads struct fields |
| `control.rs` | Yes | Cleaned messages returned as typed vec, not JSON |
| `CLAUDE.md` | Yes | New rule |

## Success Criteria
- [ ] `build_request_body` has no `body: &Value` parameter
- [ ] Message/system extraction uses typed structs, not raw JSON
- [ ] Split-path accesses typed struct fields, not `.get("input")`
- [ ] `cargo check`, `cargo fmt --check`, `cargo clippy --locked -- -D warnings` pass
- [ ] All 184 tests pass
- [ ] CLAUDE.md rule added
