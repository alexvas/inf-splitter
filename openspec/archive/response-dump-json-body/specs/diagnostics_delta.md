# Delta: Diagnostics

**Change ID:** `response-dump-json-body`
**Affects:** streaming response dump body format, SSE buffer parsing

---

## MODIFIED

### Requirement: Streaming Response Dump Body Format

Streaming response dumps must store `body` as a JSON array of parsed SSE events, not as a raw string.

#### Scenario: Successful SSE stream parsed to JSON array
- GIVEN a streaming interactions response producing SSE events
- AND each SSE event has a `data:` field containing valid JSON
- WHEN the stream ends and `response_dump_streaming` is called
- THEN `body` in the dump is a JSON array like `[{...}, {...}, ...]`
- AND each element is a parsed JSON object from the corresponding `data:` line
- AND the array elements appear in stream order

#### Scenario: Truncated SSE buffer
- GIVEN the accumulated SSE buffer was truncated at `MAX_STREAMING_DUMP_BYTES`
- AND the last SSE event is incomplete (no trailing `\n\n`)
- WHEN the buffer is parsed for the dump
- THEN the incomplete trailing event is discarded
- AND all complete preceding events are included in the JSON array

#### Scenario: Non-JSON data field
- GIVEN an SSE event with `data:` that is not valid JSON (e.g., `data: [DONE]`)
- WHEN the buffer is parsed
- THEN that event is skipped (not included in the array)

#### Scenario: Fallback on parse failure
- GIVEN the SSE buffer that cannot be parsed at all (e.g., no SSE events found)
- WHEN the buffer is parsed
- THEN the body is stored as the original string (current behavior, graceful degradation)

---

## ADDED

### Requirement: parse_sse_buffer_to_json_array Helper

A helper function that converts a raw SSE byte buffer into a `DumpBody` containing a JSON array of parsed events.

#### Scenario: Two complete SSE events
- GIVEN buffer = `data: {"a":1}\n\ndata: {"b":2}\n\n`
- WHEN `parse_sse_buffer_to_json_array(&buffer)` is called
- THEN returns `DumpBody::Utf8("[{\"a\":1},{\"b\":2}]")`

#### Scenario: Empty buffer
- GIVEN buffer = `""` (empty)
- WHEN `parse_sse_buffer_to_json_array(&buffer)` is called
- THEN returns `DumpBody::Utf8("[]")`

#### Scenario: Only non-JSON data lines
- GIVEN buffer = `data: [DONE]\n\n`
- WHEN `parse_sse_buffer_to_json_array(&buffer)` is called
- THEN returns `DumpBody::Utf8("[]")` (all skipped, empty array)
