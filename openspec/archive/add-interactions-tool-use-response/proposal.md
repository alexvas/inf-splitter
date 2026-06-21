# Proposal: Add Interactions Tool-Use Response Translation

**Change ID:** `add-interactions-tool-use-response`
**Created:** 2026-06-21
**Status:** Draft

---

## Problem Statement

When Gemini models return `function_call` steps (status: `"requires_action"`), the interactions handler does not translate them back to the client's protocol. Tool calls are silently dropped:

1. **Non-streaming path**: `extract_interaction_text()` only reads `ModelOutputStep` — `FunctionCallStep` is ignored. The response built in `send_and_translate` and `handle_split_send` always has `stop_reason: "end_turn"` with text-only content, even when the interaction's actual status is `"requires_action"`.

2. **Streaming path**: `translate_stream_event()` has no handlers for function_call events:
   - `StepStart` with `step.type: "function_call"` emits a generic text `content_block_start` instead of a `tool_use` block
   - `StepDelta` with `FunctionCallDelta` falls through to the `other` arm and is logged as `"unhandled step.delta type, dropping"` — the client never sees the tool call
   - No `tool_use` block start/delta nor `input_json_delta` events are ever emitted

**Real-world symptom (from dump):**
```json
{
  "status": "requires_action",
  "steps": [
    {"type": "thought", "signature": "..."},
    {"type": "function_call", "id": "uz1vZoUk", "name": "Agent", "arguments": {...}}
  ]
}
```
Client receives a plain text completion instead of a `tool_use` block, so the `/code-review` slash command appears to "do nothing".

## Proposed Solution

### Non-streaming: translate `FunctionCallStep` to tool_use/tool_calls blocks

Add `extract_interaction_tool_calls()` that collects `FunctionCallStep` entries from `Interaction.steps` and converts them:
- **Anthropic**: `ContentBlock::ToolUse { id, name, input }`
- **OpenAI**: `ChatContent::ToolCalls(Vec<ToolCall>)` with `ToolCall { id, function: ToolCallFunction { name, arguments } }`

Update the response construction in `send_and_translate`, `handle_split_send`, and `send_split_system_instruction` to include tool_use blocks when the interaction has `status: "requires_action"` and contains function_call steps. Set `stop_reason` accordingly.

### Streaming: handle function_call SSE events

Add cases in `translate_stream_event`:
- **StepStart (function_call)**: emit `ContentBlockStart { content_block: ToolUse { id, name, input: {} } }`
- **StepDelta (FunctionCallDelta)**: emit `ContentBlockDelta { delta: InputJsonDelta { partial_json } }` with the accumulated arguments JSON string

For the OpenAI streaming path, the existing `ReverseStreamingTranslator` should handle tool_use blocks automatically once we emit them.

### Extract tool_use blocks as typed structs

The response translation builds typed `ContentBlock` and `ToolCall` structs — no raw `json!()` for response construction.

## Scope

### In Scope
- Non-streaming: extract `FunctionCallStep` from interaction steps, translate to Anthropic `ToolUse` / OpenAI `ToolCall`
- Non-streaming: respect `status: "requires_action"` in response stop_reason
- Streaming: handle `step.start` with `function_call` type → emit `content_block_start` (tool_use)
- Streaming: handle `step.delta` with `function_call` delta → emit `content_block_delta` (input_json_delta)
- Tests for all new translation paths

### Out of Scope
- Handling other tool call types (code_execution, file_search, google_maps, google_search, url_context) — same pattern, deferred to follow-up
- Tool result submission (the second half of the tool-use round-trip) — already handled by session delta computation
- Agent mode / environment support in responses

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/interactions.rs` | Yes | Add `extract_interaction_tool_calls()`, refactor response builders to include tool_use |
| `src/interactions_handler.rs` | Yes | Handle function_call in `translate_stream_event`, wire tool calls into non-stream and split-send response construction |
| `src/interactions_types.rs` | No | `FunctionCallStep` and `FunctionCallDelta` already generated from schema |
| `build.rs` | No | No schema changes needed |

## Architecture Considerations

- The `FunctionCallStep` struct already exists in generated types with `id`, `name`, `arguments` — no schema patching needed
- The `FunctionCallDelta` struct similarly exists with `id`, `name`, `arguments`
- Non-streaming response construction is duplicated across `send_and_translate`, `handle_split_send`, and `send_split_system_instruction` — extracting a shared `build_response_from_interaction()` helper reduces duplication for the tool_use change
- Streaming path already has `translate_stream_event` as a single dispatch point — straightforward to add new match arms

## Success Criteria

- [ ] Non-streaming interactions response with `FunctionCallStep` produces Anthropic response with `tool_use` content block
- [ ] Non-streaming interactions response with `FunctionCallStep` produces OpenAI response with `tool_calls`
- [ ] Streaming `step.start` (function_call) emits `content_block_start` with tool_use type
- [ ] Streaming `step.delta` (function_call) emits `content_block_delta` with input_json_delta
- [ ] `stop_reason` reflects `"tool_use"` (Anthropic) or `"tool_calls"` (OpenAI) when status is `"requires_action"`
- [ ] `cargo fmt --check`, `cargo clippy`, `cargo test` all pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| `arguments` field type mismatch (Gemini sends object, client expects string) | Medium | Medium | `ToolChoice` serialization already handles serde_json::Value round-trips; serialize arguments as JSON string for OpenAI, pass as object for Anthropic |
| Streaming tool_use delta accumulation | Low | Low | `FunctionCallDelta.arguments` is incremental — accumulate into buffer per index, emit as `partial_json` |
| ReverseStreamingTranslator rejects tool_use events | Low | Medium | Verified: tool_use block/delta types exist in anyllm_translate 0.9.9; streaming tool_use events flow through ReverseStreamingTranslator automatically |

---

## Archive Information

**Archived:** 2026-06-21 14:07
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `build.rs` — Schema patch: `FunctionCallStep.arguments` removed from required, matching actual Gemini API step.start events
- `src/interactions.rs` — Added `extract_interaction_tool_calls()`, `build_response_from_interaction()` shared response builder; anyllm_translate imports for typed ToolUse/ToolCall construction
- `src/interactions_handler.rs` — Updated `translate_stream_event`: `StepStart` dispatches `FunctionCallStep` to `tool_use` content_block_start, `StepDelta` handles `ArgumentsDelta` as `input_json_delta`; all three non-streaming paths use shared `build_response_from_interaction()`; `guard.finish()` moved after response builder to avoid lying stats on serialization failure

### Specs Updated
- `openspec/specs/protocol-conversion.md` — MODIFIED: Interactions→Anthropic, Interactions→OpenAI; ADDED: streaming function_call scenarios, schema patching requirement
