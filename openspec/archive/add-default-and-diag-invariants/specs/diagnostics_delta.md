# Delta: Diagnostics

**Change ID:** `add-default-and-diag-invariants`
**Affects:** `src/diagnostics.rs`, `src/config.rs`, `src/interactions_handler.rs`, `src/router.rs`, `src/openai.rs`, `src/anthropic.rs`, `openspec/specs/diagnostics.md`

---

## ADDED

### Requirement: Every Protocol Handler Records Stats Events

Every protocol handler (OpenAI passthrough, Anthropic passthrough, protocol conversion, interactions) must record a `StatsEvent` for every request that produces a dump event. The invariant is: **if a request produces any dump lines, it MUST also produce a stats line, sharing the same `request_id`.**

This includes all code paths:
- Non-streaming success
- Non-streaming error
- Streaming success (recorded in spawned task)
- Streaming error
- Split-send (proxy_limit content splitting)
- System instruction splitting

#### Scenario: Non-streaming request produces stats
- GIVEN `stats_mode = "all"` and `stats_output` is set to a file path
- AND a non-streaming request arrives at any handler
- WHEN the request completes with status 200
- THEN a stats line is written with `error: null` and correct `direction`, `model`, `status`, `duration_ms`
- AND the stats line shares `request_id` with all dump lines for that request

#### Scenario: Streaming request produces stats
- GIVEN `stats_mode = "all"` and a streaming request arrives at any handler
- WHEN the stream completes
- THEN a stats line is written with `"streaming": true` and `response_size_bytes` set to the accumulated byte count
- AND the stats line is written from within the spawned stream-processing task

#### Scenario: Error request produces stats
- GIVEN `stats_mode = "error"` (or `"all"`) and the upstream returns a non-2xx status
- WHEN the error is handled
- THEN a stats line is written with `error` set to the error body text
- AND `status` matches the upstream status code

#### Scenario: Split-send produces aggregate stats
- GIVEN `stats_mode = "all"` and a request whose content exceeds `proxy_limit`
- WHEN the content is split and sent across multiple interaction chunks
- THEN one aggregate stats line is recorded after all chunks complete
- AND the stats line uses the same `request_id` as all per-chunk dump lines
- AND `response_size_bytes` is the sum of all chunk response sizes
- AND `duration_ms` covers the total elapsed time for all chunks

#### Scenario: System instruction split produces aggregate stats
- GIVEN `stats_mode = "all"` and a request where system_instruction exceeds `proxy_limit`
- WHEN the system instruction is split and sent across multiple interactions
- THEN one aggregate stats line is recorded after all interactions complete
- AND the stats line uses the same `request_id` as all per-chunk dump lines

### Requirement: StatsEvent, DumpEvent, and RouteTarget have Default

- `StatsEvent` derives `Default`. All fields default to zero/empty/None.
- `DumpBody` implements `Default` returning `Utf8(String::new())`.
- `DumpEvent` derives `Default`. All fields default to zero/empty/None/`DumpBody::default()`.
- `RouteTarget` derives `Default`. All `Option` fields default to `None`, `model_names` to empty `HashSet`, `drop_fields` to `DropFields::default()`.

#### Scenario: StatsEvent construction with defaults
- GIVEN a StatsEvent needs construction with mostly-default values
- WHEN the struct is constructed
- THEN `StatsEvent { field1, field2, ..Default::default() }` is used
- AND only non-default fields are listed explicitly

#### Scenario: RouteTarget construction in tests
- GIVEN a test needs a RouteTarget with mostly-default fields
- WHEN constructing the route
- THEN `RouteTarget { section: "test".into(), ..Default::default() }` sets only the needed fields
- AND all 10 Option fields default to `None` implicitly

### Requirement: RequestDiagnostics Session Guard

A `RequestDiagnostics` session object binds stats and dump recording into a single guard, enforcing the stats-dump parity invariant structurally.

The guard is created at request start with `Diagnostics::request(section, model)` and carries:
- A unique `request_id` (generated at construction)
- The `section` and `model` strings
- A start `Instant` for duration tracking
- Accumulated `ingress_size` and `response_size` for the eventual stats event
- A `streaming` flag
- A `finished: bool` tracking whether `finish()` or `finish_with_error()` was called

#### `Drop` safety net

If the guard is dropped without calling `finish()` or `finish_with_error()`, it logs `tracing::error!` with the request_id and records a stats event with:
- `section`, `model`, `request_id` from the guard
- `error: "diagnostics guard dropped without finish"`
- `duration_ms = start.elapsed()`
- `request_size_bytes = ingress_size`
- `status: 0`

This guarantees that every request that records dumps through the guard also gets a stats line — even if a code path forgets to call `finish()`.

#### Scenario: Normal completion
- GIVEN a `RequestDiagnostics` guard created for a request
- AND ingress/egress dumps recorded via guard methods
- WHEN `guard.finish(200, duration_ms, request_size, response_size, "upstream", "direction", false)` is called
- THEN a stats event with `error: null` is recorded
- AND the guard does not log or record anything on drop

#### Scenario: Error completion
- GIVEN a `RequestDiagnostics` guard
- AND an error response dump was recorded
- WHEN `guard.finish_with_error(502, duration_ms, request_size, error_body.len(), "upstream", "direction", error_body)` is called
- THEN a stats event with `error` set to the error body is recorded
- AND the guard does not log on drop

#### Scenario: Guard dropped without finish
- GIVEN a `RequestDiagnostics` guard
- AND `finish()` was NOT called
- WHEN the guard is dropped (e.g., early return, panic unwind)
- THEN `tracing::error!` is emitted with the request_id
- AND a stats event is recorded with `error: "diagnostics guard dropped without finish"` and `status: 0`

#### Scenario: Pilot migration preserves behavior
- GIVEN `send_and_translate` in `src/interactions_handler.rs` is migrated to use `RequestDiagnostics`
- WHEN a request completes (success or error, streaming or non-streaming)
- THEN the same stats and dump events are recorded as before the migration
- AND the `request_id` is shared across all events for that request

#### Scenario: Follow-up migration covers all handlers
- GIVEN the pilot validates `RequestDiagnostics` in `send_and_translate`
- WHEN the guard is adopted in `openai.rs` (`handle_from_openai`, `handle_sync_manual`, `handle_stream_manual`), `anthropic.rs` (`handle_from_anthropic`, `handle_from_openai`, `handle_from_openai_stream`), and `interactions_handler.rs` split paths (`handle_split_send`, `send_split_system_instruction`)
- THEN every handler function satisfies the stats-dump parity invariant structurally (via the guard, not convention)
- AND no individual `record_stats`/`record_dump` calls remain in handler code that the guard covers

### Requirement: Router-Level Client Error Visibility

Pre-routing error checks in `dispatch_messages()` (`src/router.rs`) must record a dump of the offending request body and return an `x-request-id` response header so agents and operators can correlate client errors with proxy diagnostics.

The four pre-routing error paths are:

| Error | Trigger | Fix |
|-------|---------|-----|
| Non-UTF8 body | `from_utf8(&body)` fails | Already dumps body (base64) |
| Invalid JSON | `from_str::<MessagePeek>(body_str)` fails | Add `record_request_dump` + `x-request-id` header |
| Empty model | `peek.model.trim().is_empty()` | Add `record_request_dump` + `x-request-id` header |
| Route resolution failure | `resolve_route(&peek.model)` returns `Err` | Add `record_request_dump` + `x-request-id` header |

The `x-request-id` header carries the proxy's diagnostic `request_id`, allowing an agent to log it and an operator to grep diag/dump files for full request context.

#### Scenario: Invalid JSON body is dumped and traceable
- GIVEN `dump_mode = "all"` and `stats_mode = "all"`
- AND a client sends a request body that is valid UTF-8 but not valid JSON
- WHEN the router rejects the request with 400
- THEN a stats line is written with `error` containing the serde parse error
- AND a dump line is written with `stage: "ingress"` containing the raw body
- AND both lines share the same `request_id`
- AND the HTTP response includes an `x-request-id` header with that request_id

#### Scenario: Empty model is dumped and traceable
- GIVEN a client sends `{"model": ""}` or `{"model": "   "}`
- WHEN the router rejects the request with 400
- THEN a dump line is written with the raw body
- AND the response includes `x-request-id` header

#### Scenario: Unknown model is dumped and traceable
- GIVEN a client sends `{"model": "nonexistent-model", ...}`
- AND no config section matches `"nonexistent-model"` and no catch-all exists
- WHEN the router rejects the request with 400
- THEN a dump line is written with `model` set to the unknown model name
- AND the response includes `x-request-id` header

## MODIFIED

### Requirement: Every Protocol Handler Records Dump Events

*(Add a cross-reference to the new stats parity requirement.)*

The dump-coverage invariant (every handler records dumps) is complemented by the stats-coverage invariant: **every request that produces dump lines MUST also produce a stats line with the same `request_id`.** When adding a new code path that records dumps, ensure a corresponding `record_stats` call is also present.

#### Scenario: Shared request_id between dump and stats (unchanged)
- GIVEN `dump_mode = "all"` and `stats_mode = "all"`
- AND a request completes via any handler
- WHEN both dump and stats events are recorded
- THEN all dump lines and the stats line for that request share the same `request_id`

## REMOVED

(None)
