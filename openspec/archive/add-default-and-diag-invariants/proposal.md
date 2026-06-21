# Proposal: Add Default implementations and formalize diag-dump invariants

**Change ID:** `add-default-and-diag-invariants`
**Created:** 2026-06-21
**Status:** Implementation Complete
**Archived:** 2026-06-21

---

## Archive Information

**Duration:** 1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/diagnostics.rs` — `Default` derives for `StatsEvent`, `DumpEvent`; `impl Default` for `DumpBody`; `RequestDiagnostics` guard with `Drop` safety net
- `src/config.rs` — `#[derive(Default)]` for `RouteTarget`
- `src/interactions_handler.rs` — `..Default::default()` at 8 StatsEvent sites; pilot `RequestDiagnostics` migration in `send_and_translate`
- `src/router.rs` — `..Default::default()` at 4 StatsEvent sites; dump recording for 3 error paths (JSON parse, empty model, route resolution)
- `src/openai.rs` — `..Default::default()` at 7 StatsEvent sites
- `src/anthropic.rs` — `..Default::default()` at 7 StatsEvent sites
- `src/interactions.rs` — `..Default::default()` at 1 RouteTarget test site
- `src/lib.rs` — `..Default::default()` at 1 RouteTarget test site
- `src/relay.rs` — `..Default::default()` at 1 RouteTarget test site; removed unused `HashSet` import
- `openspec/specs/diagnostics.md` — 3 new requirements: Default for structs, RequestDiagnostics guard, Router-level client error visibility
- `tests/protocol_conversion.rs` — 7 new integration tests

### Specs Updated
- `openspec/specs/diagnostics.md` — merged "StatsEvent/DumpEvent/RouteTarget Default", "RequestDiagnostics Session Guard", "Router-Level Client Error Visibility"

### Deferred
- Phase 3+: Full handler migration to `RequestDiagnostics` — guard doesn't fit multi-chunk, relay-interaction, or conditional-diagnostics patterns
- x-request-id response header — requires `AppError` refactoring to carry `request_id` through error-to-response conversion

---

## Problem Statement

Two systemic quality issues were discovered during diagnostics work on the interactions handler:

### 1. Boilerplate struct construction

`StatsEvent` is constructed 27 times across 5 files with massive repetition — every construction lists all 16 fields, including 6 `None`-valued Option fields that could trivially default. Similarly, `RouteTarget` test constructions repeat 12 `None` fields at 6 sites. This violates DRY and makes adding new fields expensive.

### 2. Missing stats (diag) recording

The recent dump fix (commit 25c75fd) added dump recording to the interactions handler's split paths (`handle_split_send`, `send_split_system_instruction`), but these paths never recorded stats. The dump file `dump-gemini.ndjson` grew to 2.1M while the stats file `diag-gemini.ndjson` stayed at 24K — a silent invariant violation. This happened because there is no enforcement mechanism coupling stats to dumps.

Root cause: the project has a formal spec requirement "Every Protocol Handler Records Dump Events" but no corresponding "Every Protocol Handler Records Stats Events." Dump recording was audited and fixed; stats recording was not.

## Proposed Solution

### Part 1: Default implementations (already staged)

- `StatsEvent` — `#[derive(Default)]`
- `DumpBody` — manual `impl Default` (returns `Utf8(String::new())`)
- `DumpEvent` — `#[derive(Default)]`
- `RouteTarget` — `#[derive(Default)]`

All 27 `StatsEvent` and 6 `RouteTarget` constructions now use `..Default::default()`, removing ~180 lines of boilerplate.

### Part 2: Missing diag tests (red-green: tests first, let them fail)

Write tests covering the interactions handler's split paths BEFORE fixing the missing `record_stats` calls. The tests must fail initially (red), then pass after the staged fix adds `record_stats` to `handle_split_send` and `send_split_system_instruction` (green).

| Gap | Handler | Scenario |
|-----|---------|----------|
| Split-send stats | interactions | Content split across chunks must record aggregate stats |
| System-instruction stats | interactions | Sys-instruction split must record aggregate stats |
| Streaming stats | interactions | Streaming interactions must record `streaming: true` |
| Error stats | interactions | Interactions error must record stats (not just dumps) |
| Streaming stats | OpenAI passthrough | OpenAI passthrough streaming stats |

The staged fix (already in the working tree) adds the missing `record_stats` calls, so tests should pass immediately. The red-green discipline ensures future regressions are caught.

### Part 3: Formalize diag-dump coupling invariant

#### 3a. Spec requirement

Add a spec requirement: **"Every handler that records a dump event for a request MUST also record a stats event for that request, sharing the same request_id."** This is the inverse of the existing dump-coverage requirement.

#### 3b. RequestDiagnostics guard

Introduce a `RequestDiagnostics` session guard that binds stats and dump recording together, making the invariant structural rather than convention-based.

**Design:**

```rust
/// Created at request start. On drop, records the stats event if `finish()` was not called.
/// Carries request_id, section, model, and accumulated timing/byte counts.
pub struct RequestDiagnostics {
    diagnostics: Diagnostics,
    request_id: String,
    section: String,
    model: String,
    start: Instant,
    /// Set by `finish()` or `finish_with_error()`. If None at drop, an "incomplete" stats event is recorded.
    finished: bool,
}
```

**Key methods:**

- `new(diagnostics, section, model)` → generates request_id, records start time
- `ingress_dump(body, headers)` — records ingress request dump
- `egress_dump(body, headers)` — records egress request dump
- `response_dump(body, status, is_error)` — records response dump (for non-streaming)
- `response_dump_streaming(body, status)` — records streaming response dump
- `finish(status, duration_ms, request_size, response_size, streaming, ...)` — records success stats event, sets `finished = true`
- `finish_with_error(status, duration_ms, ..., error)` — records error stats event, sets `finished = true`

**Safety on drop:**

If neither `finish()` nor `finish_with_error()` was called by the time the guard is dropped, a warning is logged and a best-effort stats event is recorded with `error: "incomplete"`. This ensures no stats event is silently lost even if a code path forgets to call `finish()`.

**Pilot:** Convert `interactions_handler.rs` `send_and_translate`. The guard replaces ~6 individual `record_request_dump`/`record_response_dump`/`record_stats` calls with 4-5 method calls on the guard. Validate with existing tests, then expand.

### Part 3+: RequestDiagnostics follow-up — Anthropic and OpenAI handlers

After the pilot validates the guard in `send_and_translate`, migrate the remaining handlers:

| Handler | Function | Complexity |
|---------|----------|------------|
| Anthropic passthrough | `handle_from_anthropic` | Low — 2 stats + 4 dump calls |
| Anthropic translated | `handle_from_openai` + `handle_from_openai_stream` | Medium — 3 stats + 8 dump calls |
| OpenAI passthrough | `handle_from_openai` | Low — 2 stats + 4 dump calls |
| OpenAI translated | `handle_sync_manual` + `handle_stream_manual` | Medium — 3 stats + 8 dump calls |
| Interactions split-paths | `handle_split_send` + `send_split_system_instruction` | High — multi-chunk with aggregate stats |

The guard ensures every migrated handler automatically satisfies the stats-dump parity invariant. Regression risk is low because the guard's behavior is identical to the existing individual calls — it's a pure refactoring.

### Part 4: Client error visibility (router-level errors)

`dispatch_messages()` in `src/router.rs` has four pre-routing error checks. Only one records a dump of the offending body:

| Error path | Line | Stats | Dump | Client sees |
|------------|------|-------|------|-------------|
| Non-UTF8 body | 207 | Yes | Yes | `400` + `"non-utf8"` |
| Invalid JSON | 242 | Yes | **No** | `400` + serde error |
| Empty model | 259 | Yes | **No** | `400` + `"model must not be empty"` |
| Route resolution failure | 276 | Yes | **No** | `400` + `"no route for model '...'"` |

For paths 2-4, the raw request body is not dumped. An operator debugging "why are requests failing?" has the error string in stats but not the actual bytes that caused it — making truncated JSON, typos in model names, or missing fields undebuggable.

**Fix:** Add `record_request_dump` to the three missing paths (~6 lines each). Hoist `request_id` generation before `record_stats` so it can be shared.

**Agent visibility:** Return `x-request-id` response header on all 4xx routing errors. The agent logs this header, and the operator greps diag/dump files for the request_id to find full context. Zero-risk — response headers don't affect existing clients.

Example after fix — a truncated JSON body gets both a dump and a traceable request_id:

```json
// diag-unknown.ndjson (stats):
{"section":"?","request_id":"1718570000-42","ts":"...","direction":"openai","model":"?","status":400,"request_size_bytes":89,"error":"invalid JSON body: EOF while parsing... at line 1 column 45"}

// dump-unknown.ndjson (dump):
{"section":"?","request_id":"1718570000-42","ts":"...","stage":"ingress","direction":"request","model":"?","headers":[...],"body":"{\"model\": \"gpt-4\", \"messages\": [{\"role\": \"user\", \"content\": \"hel","status":null}

// Response to client:
HTTP/1.1 400 Bad Request
x-request-id: 1718570000-42
content-type: application/json

{"type":"error","error":{"type":"invalid_request_error","message":"invalid JSON body: EOF while parsing..."}}
```

## Scope

### In Scope
- Add `Default` to `StatsEvent`, `DumpBody`, `DumpEvent`, `RouteTarget` (already staged)
- Convert all constructors to use `..Default::default()` (already staged)
- Write 5 diag tests covering interactions split-paths, streaming, and error paths (red-green discipline)
- Add spec requirement: diagnostic stats-dump parity invariant
- Implement `RequestDiagnostics` session guard with `Drop`-based finalizer
- Pilot migration: `interactions_handler.rs` `send_and_translate` → `RequestDiagnostics`
- Follow-up migration: Anthropic and OpenAI handlers → `RequestDiagnostics`
- Add dump recording to router-level error paths (JSON parse, empty model, route resolution)
- Return `x-request-id` header on 4xx routing errors for agent correlation

### Out of Scope
- Per-section writer integration tests (complex setup, low priority)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/diagnostics.rs` | New type + derive changes | Add `Default` to 3 types; add `RequestDiagnostics` guard struct |
| `src/config.rs` | Derive change | Add `Default` to `RouteTarget` |
| 5 handler files | Constructor changes | `..Default::default()` at 33 sites |
| `src/interactions_handler.rs` | Pilot migration | `send_and_translate` → `RequestDiagnostics` |
| `src/openai.rs` | Follow-up migration | 4 handler functions → `RequestDiagnostics` |
| `src/anthropic.rs` | Follow-up migration | 3 handler functions → `RequestDiagnostics` |
| `src/router.rs` | Dump + header on errors | 3 new `record_request_dump` calls + `x-request-id` response header |
| `openspec/specs/diagnostics.md` | Delta | Add stats-dump parity, guard API, client error visibility requirements |
| `tests/protocol_conversion.rs` | New tests | 7+ integration tests |

## Success Criteria
- [ ] All struct Default impls compile and pass clippy
- [ ] All 27 StatsEvent + 6 RouteTarget constructors use `..Default::default()`
- [ ] 5 diag tests pass (validate stats-dump parity in interactions split/streaming/error paths)
- [ ] Spec requirement added: every dump has a matching stats event
- [ ] `RequestDiagnostics` guard type implemented with Drop safety net
- [ ] `send_and_translate` migrated to `RequestDiagnostics` as pilot
- [ ] Anthropic and OpenAI handlers migrated to `RequestDiagnostics`
- [ ] Router-level errors (JSON parse, empty model, route resolution) record body dumps
- [ ] `x-request-id` header returned on 4xx routing errors
- [ ] All existing 269 tests still pass
- [ ] `cargo fmt --check`, `cargo clippy`, `cargo test` all pass
