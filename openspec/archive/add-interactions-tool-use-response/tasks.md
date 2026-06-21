# Implementation Tasks: Add Interactions Tool-Use Response Translation

**Change ID:** `add-interactions-tool-use-response`

---

## Phase 1: Non-Streaming Tool Call Extraction

- [x] 1.1 Add `extract_interaction_tool_calls()` in `src/interactions.rs` — collects `FunctionCallStep` from `Interaction.steps`
- [x] 1.2 Stop reason determined by interaction status inline in `build_response_from_interaction()`
- [x] 1.3 Extract `build_response_from_interaction()` shared helper in `src/interactions.rs`
- [x] 1.4 Wire tool_use blocks into non-streaming response construction for Anthropic ingress
- [x] 1.5 Wire tool_calls into non-streaming response construction for OpenAI ingress
- [x] 1.6 Unit tests: `extract_interaction_tool_calls` with function_call steps

**Quality Gate:**
- [x] `cargo test` — existing + new unit tests pass (209)
- [x] `cargo clippy` clean

---

## Phase 2: Streaming Tool Call Events

- [x] 2.1 Handle `StepStart` with `function_call` type in `translate_stream_event` → emit `content_block_start` with `tool_use` type
- [x] 2.2 Handle `ArgumentsDelta` in `translate_stream_event` → emit `content_block_delta` with `input_json_delta` type
- [x] 2.3 tool_use events flow through `ReverseStreamingTranslator` for OpenAI path (existing infra handles it)
- [x] 2.4 N/A — ReverseStreamingTranslator handles tool_use events automatically
- [x] 2.5 interaction.completed already emits correct stop events (content_block_stop + message_delta + message_stop)
- [x] 2.6 Unit tests: streaming function_call event translation

**Quality Gate:**
- [x] `cargo test` — streaming translation tests pass (209)
- [x] `cargo clippy` clean

---

## Phase 3: Response Builder Refactor & Integration

- [x] 3.1 Deduplicate non-streaming response construction into `build_response_from_interaction()` shared helper
- [x] 3.2 Apply tool_use translation to all three non-streaming paths (`send_and_translate`, `handle_split_send`, `send_split_system_instruction`)
- [x] 3.3 Integration tests pass (28 e2e + 63 protocol_conversion)
- [x] 3.4 Verify `stop_reason` is `"tool_use"` / `"tool_calls"` for requires_action interactions

**Quality Gate:**
- [x] All integration tests pass (300 total)
- [x] `cargo fmt --check` clean
- [x] `cargo clippy --locked -- -D warnings` clean

---

## Phase 4: Polish

- [x] 4.1 No `"unhandled step.delta type"` warnings for function_call deltas (uses `ArgumentsDelta` variant)
- [x] 4.2 Edge case: interaction with mixed steps handled — text extracted alongside tool calls
- [x] 4.3 Schema patch: `FunctionCallStep.arguments` made optional in build.rs for step.start events

**Quality Gate:**
- [x] Full suite: `cargo fmt --check && cargo clippy --locked -- -D warnings && cargo test --locked` — all pass
- [x] ready for `/openspec-archive`

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Specs merged (delta spec written)
- [x] Ready for `/openspec-archive`
