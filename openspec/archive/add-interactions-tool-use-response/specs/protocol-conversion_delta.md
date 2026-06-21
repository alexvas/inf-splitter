# Delta: Protocol Conversion

**Change ID:** `add-interactions-tool-use-response`
**Affects:** Interactions → Anthropic/OpenAI response translation

---

## ADDED

### Requirement: Interactions Tool-Use Response Translation (Non-Streaming)

When an `Interaction` response has `status: "requires_action"` and contains `FunctionCallStep` entries, the handler translates them to the client's protocol:

- **Anthropic ingress**: response `content[]` includes `ContentBlock::ToolUse { id, name, input }` for each function call. `stop_reason` is `"tool_use"`.
- **OpenAI ingress**: response `choices[].message.tool_calls` includes `ToolCall { id, function: { name, arguments } }` for each function call. `finish_reason` is `"tool_calls"`.

#### Scenario: Anthropic non-streaming function_call response
- GIVEN Gemini returns `Interaction` with `status: "requires_action"` and a `FunctionCallStep { id: "call-1", name: "get_weather", arguments: {"location": "Boston"} }`
- WHEN the response is translated to Anthropic format
- THEN the `MessageResponse.content` contains `ContentBlock::ToolUse { id: "call-1", name: "get_weather", input: {"location": "Boston"} }`
- AND `stop_reason` is `"tool_use"`

#### Scenario: OpenAI non-streaming function_call response
- GIVEN Gemini returns `Interaction` with `status: "requires_action"` and a `FunctionCallStep { id: "call-1", name: "get_weather", arguments: {"location": "Boston"} }`
- WHEN the response is translated to OpenAI format
- THEN `choices[0].message.tool_calls` contains `[{id: "call-1", function: {name: "get_weather", arguments: "{\"location\":\"Boston\"}"}}]`
- AND `finish_reason` is `"tool_calls"`

#### Scenario: Interaction with both text and function_call steps
- GIVEN an `Interaction` with both `ModelOutputStep` (text) and `FunctionCallStep`
- WHEN the response is translated
- THEN the response content includes both the text and tool_use/tool_calls blocks

#### Scenario: Completed interaction (no function calls) unchanged
- GIVEN `Interaction` with `status: "completed"` and only `ModelOutputStep`
- WHEN the response is translated
- THEN behavior is identical to before: text-only content, `stop_reason: "end_turn"`

### Requirement: Interactions Tool-Use Response Translation (Streaming)

Streaming SSE events for function calls are translated to Anthropic streaming events:

- `step.start` with `function_call` type → `content_block_start` with `tool_use` type block containing `id` and `name`
- `step.delta` with `function_call` delta → `content_block_delta` with `input_json_delta` type, `partial_json` set to the accumulated arguments
- `step.stop` at the function_call index → `content_block_stop`

For OpenAI streaming, these tool_use events flow through `ReverseStreamingTranslator` to produce OpenAI-format `tool_calls` chunks.

#### Scenario: Streaming function_call step.start
- GIVEN SSE event `{"event_type":"step.start","index":1,"step":{"type":"function_call","id":"call-1","name":"get_weather"}}`
- WHEN translated
- THEN emits `content_block_start` with `index: 1`, `content_block: {type: "tool_use", id: "call-1", name: "get_weather", input: {}}`

#### Scenario: Streaming function_call step.delta
- GIVEN SSE event `{"event_type":"step.delta","index":1,"delta":{"type":"function_call","id":"call-1","name":"get_weather","arguments":{"location":"Boston"}}}`
- WHEN translated
- THEN emits `content_block_delta` with `index: 1`, `delta: {type: "input_json_delta", partial_json: "{\"location\":\"Boston\"}"}`

#### Scenario: Streaming function_call step.stop
- GIVEN SSE event `{"event_type":"step.stop","index":1}`
- WHEN translated
- THEN emits `content_block_stop` with `index: 1`

#### Scenario: Function call delta accumulation
- GIVEN multiple `step.delta` events for the same function_call index
- WHEN arguments arrive incrementally
- THEN each delta emits `partial_json` with the latest accumulated state

### Requirement: Shared Response Builder for Interactions

Non-streaming response construction is extracted into a shared helper used by all three paths (`send_and_translate`, `handle_split_send`, `send_split_system_instruction`) to eliminate duplication.

#### Scenario: All three non-streaming paths use the shared builder
- GIVEN an `Interaction` and ingress protocol
- WHEN any of the three non-streaming paths constructs a response
- THEN the same `build_response_from_interaction()` function produces the translated JSON
- AND tool_use blocks are included in all paths

---

## MODIFIED

### Requirement: Interactions → Anthropic Translation

`Interaction` response translates to Anthropic `MessageResponse`:
- `Interaction.steps[]` → Anthropic `content[]` blocks: text from `ModelOutputStep`, tool_use from `FunctionCallStep`
- `Interaction.status` → `stop_reason`: `"end_turn"` for `"completed"`, `"tool_use"` for `"requires_action"`
- `Interaction.usage` → response usage metadata

### Requirement: Interactions → OpenAI Translation

`Interaction` → OpenAI `ChatCompletionResponse`:
- `Interaction.steps[]` → `choices[].message`: text from `ModelOutputStep` → `content`, `FunctionCallStep` → `tool_calls`
- `Interaction.status` → `finish_reason`: `"stop"` for `"completed"`, `"tool_calls"` for `"requires_action"`
