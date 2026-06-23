# Spec: Diagnostics

Component: `src/diagnostics.rs`

## Requirement: Diagnostic Configuration

The optional `[diagnostics]` TOML section controls stats and dump collection:

| Field | Values | Default |
|-------|--------|---------|
| `stats_output` | `"stderr"`, `"stdout"`, file path, `{per_section = path}` | `"stderr"` |
| `dump_output` | `"stderr"`, `"stdout"`, file path, `{per_section = path}` | `"stderr"` |
| `stats_mode` | `"off"`, `"error"`, `"all"` | `"off"` |
| `dump_mode` | `"off"`, `"error"`, `"all"` | `"off"` |
| `flush_period` | duration string (`"10s"`, `"1m"`) | flush every line |
| `max_file_size` | size string (`"100m"`) | no rotation |
| `max_rotated_size` | size string (`"5g"`) | no cleanup |
| `compression` | `"zip"`, `"bz2"`, `"7z"` | no compression |

### Scenario: All diagnostics off
- GIVEN no `[diagnostics]` section in config
- WHEN the proxy runs
- THEN no stats or dump events are collected (zero overhead)

### Scenario: Error-only mode
- GIVEN `stats_mode = "error"` and `dump_mode = "off"`
- WHEN a request completes successfully
- THEN no stats or dump line is written
- WHEN a request fails
- THEN a stats line is written, no dump

## Requirement: Stats Event Format

Each stats line is an NDJSON `StatsEvent` with fields:

```json
{
  "section": "deepseek",
  "request_id": "1718570000-0",
  "ts": "2026-06-17T14:30:25Z",
  "direction": "openai->anthropic",
  "model": "deepseek-v4-pro",
  "upstream": "https://api.deepseek.com/anthropic",
  "status": 200,
  "duration_ms": 1234,
  "request_size_bytes": 512,
  "response_size_bytes": 1024,
  "streaming": false,
  "input_messages": 3,
  "max_tokens": 4096,
  "messages_detail_ingress": [...],
  "messages_detail_egress": [...]
}
```

### Scenario: Stats serialization
- GIVEN a request has completed
- WHEN the stats event is recorded
- THEN it is written as a single NDJSON line to the configured sink

### Scenario: Optional fields omitted
- GIVEN `response_size_bytes` is `None` (streaming request)
- WHEN the event is serialized
- THEN the field is omitted from JSON output

### Scenario: Error field contains decoded text
- GIVEN upstream returns a gzip-compressed error response (e.g. Gemini API)
- WHEN the stats event is recorded
- THEN `error` contains the decompressed JSON text (reqwest with `gzip` feature auto-decompresses)

## Requirement: Dump Event Format

Each dump line is an NDJSON `DumpEvent` with fields.
Valid JSON bodies are embedded as native JSON objects/arrays; non-JSON text remains a JSON string.

```json
{
  "section": "deepseek",
  "request_id": "1718570000-0",
  "ts": "2026-06-17T14:30:25Z",
  "stage": "ingress",
  "direction": "request",
  "model": "deepseek-v4-pro",
  "headers": [["content-type", "application/json"]],
  "body": {"error": {"message": "permission denied"}},
  "status": null
}
```

### Scenario: Dump with UTF-8 body (valid JSON)
- GIVEN request/response body is valid UTF-8 that parses as JSON
- WHEN a dump event is recorded
- THEN `body` contains the embedded JSON value (not a JSON-escaped string)

### Scenario: Dump with UTF-8 body (non-JSON)
- GIVEN request/response body is valid UTF-8 but not valid JSON (e.g., "plain text error")
- WHEN a dump event is recorded
- THEN `body` contains the plain text as a JSON string

### Scenario: Dump with empty body
- GIVEN body is an empty string `""`
- WHEN a dump event is recorded
- THEN `body` is serialized as `""` (empty JSON string parsing fails, falls back to string)

### Scenario: Dump with binary body
- GIVEN request body is not valid UTF-8
- WHEN a dump event is recorded
- THEN `body` contains base64-encoded content and `"encoding": "base64"` is set

### Scenario: Binary body truncation
- GIVEN binary body exceeds `MAX_NON_UTF8_DUMP_LEN` (65536 bytes)
- WHEN a dump event is recorded
- THEN the body is truncated to 65536 bytes before base64 encoding

## Requirement: Sensitive Header Masking

Header values for `x-goog-api-key`, `authorization`, and `x-api-key` are
masked as `"***"` in all dump output. The masking is case-insensitive and
applied at the lowest level across all entry points:

- `Diagnostics::record_request_dump` — via `header_pairs_with_masking`
- `Diagnostics::record_response_dump` — via `mask_header_values`
- `RequestDiagnostics::ingress_dump` / `egress_dump` — via `header_pairs_with_masking`

This covers Router direct calls, Relay/DiagnosticStream, and all three handlers.

### Scenario: Egress dump with api_key
- GIVEN `x-goog-api-key: AIzaSy...` is set in egress headers
- WHEN the dump is recorded through ANY path
- THEN the header appears as `["x-goog-api-key", "***"]`

### Scenario: Non-sensitive headers pass through
- GIVEN `x-request-id: trace-12345` and `content-type: application/json`
- WHEN the dump is recorded
- THEN these headers appear with their original values unchanged

## Requirement: Egress and Response Dumps Use Actual Upstream Headers (All Handlers)

All `egress_dump` calls in `interactions_handler.rs`, `openai.rs`, and
`anthropic.rs` receive the actual headers sent to the upstream (after
`build_interactions_headers` / `forward_request_headers` transformation),
not the ingress `request_headers`. Two helpers enable this:

- `auth::forward_request_headers_map(api_key, request_headers) -> HeaderMap`
- `interactions_handler::build_interactions_headers_map(api_key, request_headers) -> HeaderMap`

All `response_dump` and `response_dump_streaming` calls in all handlers
receive the actual upstream response headers from `reqwest::Response::headers()`
instead of `vec![]`. The `response_dump_streaming` signature accepts a `headers:
Vec<(String, String)>` parameter. A shared helper `response_headers_to_pairs`
converts `HeaderMap` to `Vec<(String, String)>` in `interactions_handler.rs`.

### Scenario: Interactions handler with API key
- GIVEN `api_key = "some-key"` in config
- WHEN an interactions request is sent
- THEN the egress dump shows `x-goog-api-key: ***` (masked) and `Api-Revision`, `Content-Type` headers

### Scenario: OpenAI/Anthropic handler with API key
- GIVEN `api_key = "some-key"` in config
- WHEN a passthrough/conversion request is sent
- THEN the egress dump shows `x-api-key: ***` and `authorization: ***` (masked)

### Scenario: Non-streaming response dump contains upstream headers
- GIVEN an interactions request completes successfully
- AND the upstream returns headers `content-type: application/json` and `x-request-id: abc123`
- WHEN the response dump is recorded
- THEN `headers` in the dump entry contains both header pairs (non-empty array)
- AND sensitive headers are masked per existing masking rules

### Scenario: Streaming response dump contains upstream headers
- GIVEN an interactions streaming request completes
- AND the upstream returns response headers
- WHEN `response_dump_streaming` is called from the spawned task
- THEN `headers` in the dump entry contains the upstream response headers

### Scenario: Error response dump contains upstream headers
- GIVEN the upstream returns a 429 with header `retry-after: 30`
- WHEN the error is handled and a response dump is recorded
- THEN `headers` in the dump entry contains `retry-after: 30`

### Scenario: Anthropic handler error path includes response headers
- GIVEN an Anthropic passthrough/conversion request fails with upstream error
- AND the upstream response has headers
- WHEN the error path records a response dump
- THEN `headers` contains the upstream response headers (not `[]`)

### Scenario: OpenAI handler error path includes response headers
- GIVEN an OpenAI passthrough/conversion request fails with upstream error
- AND the upstream response has headers
- WHEN the error path records a response dump
- THEN `headers` contains the upstream response headers (not `[]`)

## Requirement: proxy_limit Size Check Uses Full Request Body

The proxy_limit size check in `interactions_handler.rs` measures the full
serialized `CreateModelInteractionParams` body, including `system_instruction`,
`tools`, and all other fields — not just the `input` ContentList.

### Scenario: Small input but large system_instruction
- GIVEN `proxy_limit = "100k"` and a request with 10K input but 120K system_instruction
- WHEN the request is processed
- THEN the full body exceeds 100K limit and splitting is triggered

## Requirement: Every Protocol Handler Records Dump Events

Every protocol handler (OpenAI passthrough, Anthropic passthrough, protocol conversion, interactions) must record the same categories of dump events for every request:

- **ingress request** — the original client body as received by the proxy
- **egress request** — the body actually sent upstream (after token caps, protocol translation, control message stripping, etc.)
- **egress response** — the raw upstream response body (up to 1 MiB for streaming)

All dump events for a single request share the same `request_id` as the corresponding stats event.

### Scenario: Non-streaming request produces dump
- GIVEN `dump_mode = "all"` and `dump_output` is set to a file path
- AND a non-streaming request arrives at any handler
- WHEN the request completes with status 200
- THEN three dump lines are written: ingress request, egress request, egress response
- AND all three lines share the same `request_id`

### Scenario: Streaming request produces dump
- GIVEN `dump_mode = "all"` and `dump_output` is set to a file path
- AND a streaming request arrives at any handler
- WHEN the stream completes with status 200
- THEN ingress and egress request dump lines are written
- AND a response dump line is written with the raw body (up to 1 MiB)

### Scenario: Error response produces dump
- GIVEN `dump_mode = "all"` (or `"error"`) and `dump_output` is set to a file path
- AND the upstream returns a non-2xx status
- WHEN the error is handled via `finish_with_upstream_error`
- THEN ingress and egress request dump lines are written
- AND a response dump line is written containing the error body
- AND the response dump includes the upstream response headers

### Scenario: finish_with_upstream_error guarantees response dump
- GIVEN any handler receiving an upstream HTTP error
- WHEN the handler calls `finish_with_upstream_error` (not bare `finish_with_error`)
- THEN a response dump is guaranteed by the method itself — the two-call pattern is eliminated
- AND the invariant holds for all handlers (passthrough, conversion, interactions, split-send)

### Scenario: No dump when dump_mode is off
- GIVEN `dump_mode = "off"`
- WHEN a request completes (any handler)
- THEN no dump lines are written for that request

### Scenario: Shared request_id between dump and stats
- GIVEN `dump_mode = "all"` and `stats_mode = "all"`
- AND a request completes via any handler
- WHEN both dump and stats events are recorded
- THEN all dump lines and the stats line for that request share the same `request_id`

## Requirement: Every Protocol Handler Records Stats Events

Every protocol handler (OpenAI passthrough, Anthropic passthrough, protocol conversion, interactions) must record a `StatsEvent` for every request that produces a dump event. The invariant is: **if a request produces any dump lines, it MUST also produce a stats line, sharing the same `request_id`.**

This includes all code paths:
- Non-streaming success
- Non-streaming error
- Streaming success (recorded in spawned task)
- Streaming error
- Split-send (proxy_limit content splitting)
- System instruction splitting

### Scenario: Split-send produces aggregate stats
- GIVEN `stats_mode = "all"` and a request whose content exceeds `proxy_limit`
- WHEN the content is split and sent across multiple interaction chunks
- THEN one aggregate stats line is recorded after all chunks complete
- AND the stats line uses the same `request_id` as all per-chunk dump lines
- AND `response_size_bytes` is the sum of all chunk response sizes
- AND `duration_ms` covers the total elapsed time for all chunks

### Scenario: System instruction split produces aggregate stats
- GIVEN `stats_mode = "all"` and a request where system_instruction exceeds `proxy_limit`
- WHEN the system instruction is split and sent across multiple interactions
- THEN one aggregate stats line is recorded after all interactions complete
- AND the stats line uses the same `request_id` as all per-chunk dump lines

### Scenario: Streaming request produces stats
- GIVEN `stats_mode = "all"` and a streaming request arrives at any handler
- WHEN the stream completes
- THEN a stats line is written with `"streaming": true` and `response_size_bytes` set to the accumulated byte count
- AND the stats line is written from within the spawned stream-processing task

### Scenario: Error request produces stats
- GIVEN `stats_mode = "error"` (or `"all"`) and the upstream returns a non-2xx status
- WHEN the error is handled
- THEN a stats line is written with `error` set to the error body text
- AND `status` matches the upstream status code
- AND the dump lines and stats line share the same `request_id`

## Requirement: StatsEvent, DumpEvent, and RouteTarget have Default

- `StatsEvent` derives `Default`. All fields default to zero/empty/None.
- `DumpBody` implements `Default` returning `Utf8(String::new())`.
- `DumpEvent` derives `Default`. All fields default to zero/empty/None/`DumpBody::default()`.
- `RouteTarget` derives `Default`. All `Option` fields default to `None`, `model_names` to empty `HashSet`, `drop_fields` to `DropFields::default()`.

### Scenario: StatsEvent construction with defaults
- GIVEN a StatsEvent needs construction with mostly-default values
- WHEN the struct is constructed
- THEN `StatsEvent { field1, field2, ..Default::default() }` is used
- AND only non-default fields are listed explicitly

### Scenario: RouteTarget construction in tests
- GIVEN a test needs a RouteTarget with mostly-default fields
- WHEN constructing the route
- THEN `RouteTarget { section: "test".into(), ..Default::default() }` sets only the needed fields

## Requirement: RequestDiagnostics Session Guard (v2)

A `RequestDiagnostics` session object binds stats and dump recording into a single guard, enforcing the stats-dump parity invariant structurally. Created at request start with `RequestDiagnostics::new(diagnostics, section, model)`. All methods take `&self`; the guard uses `Mutex` for interior mutability and is `Send + Sync`.

The guard struct (`StoredDump` is `(DumpBody, Vec<(String, String)>, String, Option<u16>)` — body, headers, capture timestamp, response status):

```rust
pub struct RequestDiagnostics {
    diagnostics: Diagnostics,
    request_id: String,
    section: String,
    model: String,
    start: Instant,
    finished: Mutex<bool>,
    ingress_size: Mutex<usize>,
    input_messages: Mutex<Option<usize>>,
    max_tokens: Mutex<Option<u32>>,
    messages_detail_ingress: Mutex<Option<serde_json::Value>>,
    messages_detail_egress: Mutex<Option<serde_json::Value>>,
    ingress_dump_pending: Mutex<Option<StoredDump>>,
    egress_dumps_pending: Mutex<Vec<StoredDump>>,
    response_dump_pending: Mutex<Option<StoredDump>>,
}
```

**Methods:**
- `request_id()` — returns `&str`
- `ingress_size()` — returns `usize`
- `model()` — returns `&str`
- `section()` — returns `&str`
- `diagnostics_handle()` — returns `Diagnostics` clone
- `set_input_messages(n)`, `set_max_tokens(n)`, `set_messages_detail_ingress(v)`, `set_messages_detail_egress(v)` — optional stats detail setters
- `ingress_dump(body, headers)` — stores ingress dump with capture-time timestamp for deferred recording
- `egress_dump(body, headers)` — stores egress dump with capture-time timestamp for deferred recording
- `response_dump(body, status, is_error, headers)` — stores response dump for deferred recording (flushed in `finish`/`finish_with_error`)
- `response_dump_streaming(body, status)` — stores streaming response dump for deferred recording
- `finish(status, duration_ms, request_size, response_size, upstream, direction, streaming)` — records success stats, flushes all deferred dumps (ingress, egress, response) with `is_error: false`, idempotent
- `finish_with_error(status, duration_ms, request_size, response_size, upstream, direction, streaming, error)` — records error stats, flushes all deferred dumps with `is_error: true`, idempotent
- `finish_with_upstream_error(status, duration_ms, request_size, upstream, direction, streaming, error_body, response_headers)` — records a response dump with the upstream error body, then calls `finish_with_error`. Replaces the two-call `response_dump` + `finish_with_error` pattern for upstream HTTP errors, guaranteeing the response dump is never forgotten. Internal errors (no HTTP response body) continue to use `finish_with_error` directly.

**Drop safety net:** If dropped without `finish()`/`finish_with_error()`, logs `tracing::error!` and records a stats event with `error: "diagnostics guard dropped without finish"`.

### Scenario: Normal completion
- GIVEN a `RequestDiagnostics` guard created for a request
- AND ingress/egress dumps stored via guard methods
- WHEN `guard.finish(200, ...)` is called
- THEN deferred dumps are flushed with `is_error: false`
- AND a success stats event is recorded
- AND the guard does not log on drop

### Scenario: Guard dropped without finish
- GIVEN a `RequestDiagnostics` guard
- AND `finish()` was NOT called
- WHEN the guard is dropped
- THEN `tracing::error!` is emitted
- AND deferred dumps are flushed with `is_error: true`
- AND a stats event is recorded with `error: "diagnostics guard dropped without finish"` and `status: 0`

### Scenario: All handler patterns use the guard
- GIVEN any protocol handler (passthrough, translation, interactions, split-send)
- WHEN a request is processed
- THEN `RequestDiagnostics` is created once at request start
- AND all dumps and the stats event share the same `request_id`

### Scenario: Multi-chunk split-send
- GIVEN a request whose content exceeds `proxy_limit`
- WHEN the content is split into N chunks and sent sequentially
- THEN the guard records one ingress dump, N egress dumps, N response dumps
- AND `guard.finish()` records one aggregate stats event with `response_size_bytes` = sum of all chunk responses
- AND all events share the same `request_id`

### Scenario: Per-chunk error with early return
- GIVEN a split-send where chunk 2 of 5 fails
- WHEN the upstream returns an error for chunk 2
- THEN `guard.finish_with_error()` records an error stats event
- AND the function returns early
- AND subsequent `guard.drop()` is a no-op (already finished)

### Scenario: Relay-interaction with shared request_id
- GIVEN a passthrough handler using `RequestDiagnostics`
- WHEN the error path records stats via `guard.finish_with_error()`
- OR the success path relays through `relay_*_upstream` and records stats via `guard.finish()`
- THEN both paths use the same `request_id` (created once at guard construction)
- AND the relay function uses `guard` for response dump recording

### Scenario: Conditional diagnostics
- GIVEN a translation handler where `dump_enabled()` is false
- WHEN the handler uses `RequestDiagnostics`
- THEN `ingress_dump`/`egress_dump` are never called (body strings are `None`)
- AND `set_input_messages`/`set_max_tokens`/`set_messages_detail_*` are only called when `stats_enabled()` is true
- AND `finish()` still records a stats event with the fields that were set

### Scenario: Streaming task moves guard by value
- GIVEN a streaming handler that spawns a `tokio::spawn` task
- WHEN the guard is `Send + Sync`
- THEN the guard is moved into the spawned task by value
- AND `guard.response_dump_streaming()` + `guard.finish()` are called inside the task
- AND no raw `diagnostics.record_*` calls remain in the spawned task

### Scenario: Client disconnect during streaming
- GIVEN an interactions stream is in progress
- WHEN the client disconnects (causing `tx.send()` to fail)
- THEN `guard.finish()` is called with status 499 and accumulated stats before `return`
- AND no `diagnostics guard dropped without finish` error is logged

### Scenario: Stream chunk error
- GIVEN an interactions stream is in progress
- WHEN the upstream stream returns an error chunk
- THEN `guard.finish_with_error()` is called with status 502 before `return`
- AND no `diagnostics guard dropped without finish` error is logged

### Scenario: Idempotent finish
- GIVEN `finish()` was already called
- WHEN `finish()` or `finish_with_error()` is called again
- THEN the call is a no-op (returns immediately)

### Scenario: Upstream HTTP error recorded with finish_with_upstream_error
- GIVEN an upstream returns a non-success HTTP status with an error body
- AND response headers are collected from the upstream response
- WHEN `finish_with_upstream_error(status, duration, size, upstream, dir, stream, error_body, headers)` is called
- THEN a response dump is recorded with `stage: "egress"`, `direction: "response"`, the error status, and the error body
- AND `finish_with_error` is called with the same status, error body, and `response_size = Some(error_body.len())`
- AND the guard is marked finished (idempotent)

### Scenario: finish_with_upstream_error includes response headers
- GIVEN upstream error response headers contain diagnostic information
- WHEN `finish_with_upstream_error` is called with those headers
- THEN the response dump includes the headers (with sensitive values masked)
- AND the headers are preserved for debugging the upstream error

### Scenario: Internal errors continue using finish_with_error directly
- GIVEN an error originates internally (validation, session, stream infrastructure — no HTTP response body exists)
- WHEN `finish_with_error` is called directly
- THEN a stats event is recorded with the error
- AND no response dump is recorded (there is no HTTP response to dump)
- AND behavior is identical to before `finish_with_upstream_error` was introduced

### Scenario: Per-dump capture-time timestamps
- GIVEN a split-send with 2 chunks sent seconds apart
- WHEN `guard.finish()` flushes deferred dumps
- THEN each egress dump has the timestamp from when it was captured by `egress_dump()`
- AND the timestamps differ from the stats event timestamp

### Scenario: Passthrough request dumps carry response status
- GIVEN an anthropic→anthropic passthrough success request
- WHEN the request completes with status 200
- THEN ingress and egress request dumps have `status: 200`

### Scenario: Control action clean-all fails
- GIVEN `handle_control_action` is called with `ControlAction::CleanAll`
- AND `session_store.remove_all()` returns an error
- WHEN the error propagates
- THEN `guard.finish_with_error()` is called BEFORE the error return
- AND a stats entry is recorded with the error message
- AND no "diagnostics guard dropped without finish" error is logged

### Scenario: Control action extend-lifetime fails
- GIVEN `handle_control_action` is called with `ControlAction::ExtendLifetime(ts)`
- AND `session_store.extend_lifetime()` returns an error
- WHEN the error propagates
- THEN `guard.finish_with_error()` is called BEFORE the error return
- AND a stats entry is recorded with the error message
- AND no "diagnostics guard dropped without finish" error is logged

### Scenario: Missing detail fields omitted from stats
- GIVEN a passthrough request body with no `messages` field
- WHEN stats are recorded
- THEN `messages_detail_ingress` is absent from the JSON output (not `null`)

## Requirement: Router-Level Client Error Visibility

Pre-routing error checks in `dispatch_messages()` must record a dump of the offending request body so operators can debug malformed client requests. All four error paths now record an ingress dump:

| Error | Trigger | Status |
|-------|---------|--------|
| Non-UTF8 body | `from_utf8(&body)` fails | Dump implemented |
| Invalid JSON | `from_str::<MessagePeek>(body_str)` fails | Dump implemented |
| Empty model | `peek.model.trim().is_empty()` | Dump implemented |
| Route resolution failure | `resolve_route(&peek.model)` returns `Err` | Dump implemented |

### Scenario: Invalid JSON body is dumped
- GIVEN `dump_mode = "all"` (or `"error"`)
- AND a client sends a request body that is valid UTF-8 but not valid JSON
- WHEN the router rejects the request with 400
- THEN a dump line is written with `stage: "ingress"` containing the raw body
- AND a stats line is written with the same `request_id`

### Scenario: Empty model is dumped
- GIVEN a client sends `{"model": ""}`
- WHEN the router rejects the request with 400
- THEN a dump line is written with the raw body

## Requirement: Timestamp Format

All diagnostic timestamps use ISO 8601 UTC format: `YYYY-MM-DDTHH:MM:SSZ`. This applies to both `StatsEvent.ts` and `DumpEvent.ts`.

### Scenario: Timestamp format
- GIVEN the current time is 2026-06-17 14:30:25 UTC
- WHEN a diagnostic event is created
- THEN `ts` is `"2026-06-17T14:30:25Z"`

## Requirement: Per-Section Output

When `stats_output` or `dump_output` uses `{per_section = path}`, separate files are created per config section.

### Scenario: Per-section file naming
- GIVEN `dump_output = {per_section = "/var/log/dump.ndjson"}` and section `deepseek`
- WHEN a dump event is recorded for that section
- THEN it writes to `/var/log/dump-deepseek.ndjson`

## Requirement: Non-Blocking Writes

All diagnostic recording uses `try_send` on bounded MPSC channels. When the channel is full, events are silently dropped — never blocking request processing.

### Scenario: Backpressure handling
- GIVEN the diagnostic writer is slow (disk I/O)
- WHEN the MPSC channel fills up (1024 capacity)
- THEN new events are dropped without affecting request latency

## Requirement: File Rotation

`RotatingWriter::flush()` calls `BufWriter::flush()` followed by `File::sync_data()` (Linux `fdatasync`), ensuring data reaches the storage device.

When `max_file_size` is set, the current output file is rotated when it exceeds the limit. Rotated files are named with a date-sequence suffix. When `compression` is set, rotated files are compressed in a background thread. When `max_rotated_size` is set, oldest rotated files are deleted when total exceeds the limit.

### Scenario: Rotation triggered
- GIVEN `max_file_size = "100m"` and current file reaches 100 MiB
- WHEN the next line is written
- THEN the current file is renamed and a new file is started

### Scenario: Compression after rotation
- GIVEN `compression = "zip"` and a file has been rotated
- WHEN rotation completes
- THEN the rotated file is compressed to `.ndjson.zip` in a background thread

### Scenario: 7z compression after rotation
- GIVEN `compression = "7z"` and `max_file_size` is set
- WHEN a dump file is rotated
- THEN the rotated file is compressed to `.ndjson.7z`
- AND the original `.ndjson` file is removed

### Scenario: Bz2 compression after rotation
- GIVEN `compression = "bz2"` and `max_file_size` is set
- WHEN a dump file is rotated
- THEN the rotated file is compressed to `.ndjson.bz2`
- AND the original `.ndjson` file is removed

### Scenario: Compression failure preserves original
- GIVEN any compression is configured
- AND the compression fails (e.g., disk full)
- WHEN compression is attempted
- THEN the original `.ndjson` file is preserved
- AND an error is logged

### Scenario: Old file cleanup
- GIVEN `max_rotated_size = "1g"` and rotated files total 1.2 GiB
- WHEN rotation occurs
- THEN the oldest rotated files are deleted until total is under 1 GiB

## Requirement: Every Protocol Handler Records Dump and Stats Events

All four handlers (OpenAI passthrough, Anthropic passthrough, protocol conversion, interactions) must record an ingress dump and a stats event for every request — including early error paths.

### Scenario: Interactions proxy_limit split check fails

- GIVEN an Anthropic or OpenAI ingress request routed to the interactions handler
- AND the request size exceeds `proxy_limit`
- AND EITHER `can_split_under_limit` determines the request cannot be split
- OR `pack_content_into_chunks` fails because a single content item exceeds the limit
- OR `split_text_for_limit` fails because system_instruction cannot be split under the limit
- WHEN the handler returns a 400 error
- THEN an ingress dump is written to `dump_output`
- AND a stats entry is written to `stats_output` with `status: 400` and the full error message in the `error` field
- AND `guard.finish_with_error(400, ...)` is called BEFORE the error return (not dropped)
- AND the client receives `400 bad request: Request cannot be split under proxy limit (see diagnostics for details)`

### Scenario: Interactions control action executed

- GIVEN an interactions request containing a control message (clean_all or extend_lifetime)
- WHEN the handler executes the control action successfully
- THEN a stats entry is written with `status: 200`, `upstream: "control-action"`, `direction` matching the action type

## Requirement: No Unfinalized Guard on Error Return

В любой функции, владеющей `RequestDiagnostics` (guard), каждый `?`-проброс ошибки ДО вызова `guard.finish()` / `guard.finish_with_error()` является нарушением инварианта. `.map_err()?` — частный случай, не менее опасный.

**Правило:** перед `return Err(...)` guard должен быть финализирован через `guard.finish_with_error(status, ..., err_msg)`. Допустимая альтернатива: явный `match` с вызовом `guard.finish_with_error()` перед `return Err(...)`.

**Проверка при code review:** если в функции есть `guard: RequestDiagnostics` и встречается `?` до строки с `guard.finish(...)` — это красный флаг.

### Scenario: send_and_translate network send failure

- GIVEN `send_and_translate` отправляет запрос в upstream
- AND `upstream.send().await` возвращает `Err(reqwest::Error)`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(502, ..., error_message)` вызывается ДО `return Err(...)`

### Scenario: send_and_translate response read failure

- GIVEN `send_and_translate` читает тело ответа
- AND `upstream.bytes().await` возвращает `Err(reqwest::Error)`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(502, ...)` вызывается ДО `return Err(...)`

### Scenario: send_and_translate body validation failure

- GIVEN `send_and_translate` валидирует тело ответа
- AND `validate_upstream_body()` возвращает `Err(AppError)`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(502, ...)` вызывается ДО `return Err(...)`

### Scenario: send_and_translate interaction parse failure

- GIVEN `send_and_translate` парсит JSON ответа как `Interaction`
- AND `serde_json::from_str()` возвращает `Err`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(502, ...)` вызывается ДО `return Err(...)`
- AND `response_body.len()` передаётся как `response_size` для диагностики

### Scenario: send_and_translate response build failure

- GIVEN `send_and_translate` собирает ingress-ответ через `build_response_from_interaction`
- AND функция возвращает `Err(String)`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(500, ...)` вызывается ДО `return Err(AppError::Internal(...))`

### Scenario: handle_split_send chunk packing failure

- GIVEN `handle_split_send` пакует контент в чанки через `pack_content_into_chunks`
- AND single content item превышает `proxy_limit`
- WHEN `pack_content_into_chunks` возвращает `Err("content item too large for proxy_limit: ...")`
- THEN `guard.finish_with_error(400, ...)` вызывается ДО `return Err(AppError::BadRequest(...))`

### Scenario: send_split_system_instruction split failure

- GIVEN `send_split_system_instruction` разбивает system_instruction через `split_text_for_limit`
- AND текст не удаётся разбить под лимит
- WHEN `split_text_for_limit` возвращает `Err`
- THEN `guard.finish_with_error(400, ...)` вызывается ДО `return Err(AppError::BadRequest(...))`

## Requirement: Envelope Size Breakdown in can_split_under_limit Errors

When the non-splittable envelope exceeds `proxy_limit`, the error message must list each contributing field with its byte count and human-readable size.

### Scenario: Tools dominate the envelope

- GIVEN a request with 105 tools totaling 160 KiB
- AND `proxy_limit` set to 100 KiB
- WHEN `can_split_under_limit` checks the envelope
- THEN the error message includes lines like:
  - `model: 19 B`
  - `stream: 4 B`
  - `tools: 160.0 KiB`

## Requirement: Per-Tool Size Breakdown in can_split_under_limit Errors

When `can_split_under_limit` returns an error, and tools are present, the error message must include a per-tool size breakdown sorted by total size descending, showing name, total serialized size, description size, and parameters schema size for each `Tool::Function`.

### Scenario: Real-world tool list breakdown

- GIVEN a request with 105 tools from a Claude Code session
- AND `proxy_limit` set to 100 KiB
- WHEN `can_split_under_limit` returns an error
- THEN the error message contains a section `Per-tool size breakdown (sorted by size):`
- AND each `Tool::Function` line shows `{name}: {total} (description: {desc_size}, parameters: {params_size})`
- AND non-Function tools show `({type_name}): {total}`
- AND tools are sorted by total size in descending order (heaviest first)
- AND sizes use human-readable units (B, KiB, MiB)

## Requirement: Lazy Tool Breakdown Computation

The per-tool size breakdown must only be computed when a limit error actually occurs, not on every request.

### Scenario: Request under limit

- GIVEN a request whose envelope fits within `proxy_limit`
- WHEN `can_split_under_limit` is called
- THEN `tool_size_breakdown` is never invoked
- AND no per-tool serialization overhead is incurred

## Requirement: format_bytes Helper

A `format_bytes(bytes: usize) -> String` function must format byte counts in human-readable units:

- `< 1024`: `"{n} B"`
- `< 1024*1024`: `"{n/1024:.1} KiB"`
- `>= 1024*1024`: `"{n/(1024*1024):.1} MiB"`

## Requirement: Streaming Response Dump Body Format

Streaming response dumps must store `body` as a JSON array of parsed SSE events, not as a raw string.

### Scenario: Successful SSE stream parsed to JSON array
- GIVEN a streaming interactions response producing SSE events
- AND each SSE event has a `data:` field containing valid JSON
- WHEN the stream ends and `response_dump_streaming` is called
- THEN `body` in the dump is a JSON array like `[{...}, {...}, ...]`
- AND each element is a parsed JSON object from the corresponding `data:` line
- AND the array elements appear in stream order

### Scenario: Truncated SSE buffer
- GIVEN the accumulated SSE buffer was truncated at `MAX_STREAMING_DUMP_BYTES`
- AND the last SSE event is incomplete (no trailing `\n\n`)
- WHEN the buffer is parsed for the dump
- THEN the incomplete trailing event is discarded
- AND all complete preceding events are included in the JSON array

### Scenario: Non-JSON data field
- GIVEN an SSE event with `data:` that is not valid JSON (e.g., `data: [DONE]`)
- WHEN the buffer is parsed
- THEN that event is skipped (not included in the array)

### Scenario: Fallback on parse failure
- GIVEN the SSE buffer that cannot be parsed at all (e.g., no SSE events found)
- WHEN the buffer is parsed
- THEN the body is stored as the original string (current behavior, graceful degradation)

## Requirement: parse_sse_buffer_to_json_array Helper

A helper function in `src/sse.rs` that converts a raw SSE byte buffer into a `DumpBody` containing a JSON array of parsed events.

### Scenario: Two complete SSE events
- GIVEN buffer = `data: {"a":1}\n\ndata: {"b":2}\n\n`
- WHEN `parse_sse_buffer_to_json_array(&buffer)` is called
- THEN returns `DumpBody::Utf8("[{\"a\":1},{\"b\":2}]")`

### Scenario: Empty buffer
- GIVEN buffer = `""` (empty)
- WHEN `parse_sse_buffer_to_json_array(&buffer)` is called
- THEN returns `DumpBody::Utf8("[]")`

### Scenario: Only non-JSON data lines
- GIVEN buffer = `data: [DONE]\n\n`
- WHEN `parse_sse_buffer_to_json_array(&buffer)` is called
- THEN returns `DumpBody::Utf8("[]")` (all skipped, empty array)

### Scenario: Non-UTF-8 buffer
- GIVEN buffer = `\xff\xfe\xfd` (invalid UTF-8)
- WHEN `parse_sse_buffer_to_json_array(&buffer)` is called
- THEN returns `DumpBody::Base64(...)` (fallback to base64)

## Requirement: Poll Diagnostics File Stabilization (Tests)

`poll_diagnostics_file` must wait for file content to stabilize before returning.

### Scenario: Writer still appending
- GIVEN the writer thread has written 1 of 3 pending dump lines
- AND the predicate is already satisfied
- WHEN `poll_diagnostics_file` checks the file
- THEN it waits 20ms and re-reads
- AND if the size grew, continues polling until stable
- AND returns only when consecutive reads have the same size

## Requirement: Session Store Creates Parent Directory on Save

`SessionStore::save_to_disk` must ensure the parent directory exists before writing the temporary file. It uses `std::fs::create_dir_all` on the parent of `self.path` before `fs::write`.

### Scenario: First run with missing directory
- GIVEN the session file path is `/var/lib/inf-splitter/interactions-sessions.toml`
- AND the directory `/var/lib/inf-splitter/` does not exist
- WHEN `save_to_disk` is called
- THEN `create_dir_all("/var/lib/inf-splitter/")` creates the directory
- AND the TOML file is written successfully

### Scenario: Directory already exists
- GIVEN the parent directory already exists
- WHEN `save_to_disk` is called
- THEN `create_dir_all` is a no-op
- AND the file is written normally

## Requirement: Session Update Errors Are Logged

`SessionStore::update` logs `save_to_disk` errors internally via `tracing::warn!` instead of silently discarding them. The method signature stays `Result<(), String>` for callers that do inspect the error, but the warning is already logged by the time the `Result` propagates.

### Scenario: update fails to persist
- GIVEN `session_store.update()` is called
- AND `save_to_disk` fails (e.g., disk full)
- WHEN the error occurs
- THEN `tracing::warn!` logs the session ID and error details inside `update`
- AND the `Result::Err` is still returned for callers that want to handle it
