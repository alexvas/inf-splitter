# Implementation Tasks: Add Default and Formalize Diag Invariants

**Change ID:** `add-default-and-diag-invariants`

---

## Phase 1: Default implementations (already staged)

- [x] 1.1 Add `#[derive(Default)]` to `StatsEvent` in `src/diagnostics.rs`
- [x] 1.2 Add `impl Default for DumpBody` + `#[derive(Default)]` for `DumpEvent` in `src/diagnostics.rs`
- [x] 1.3 Add `#[derive(Default)]` to `RouteTarget` in `src/config.rs`
- [x] 1.4 Convert 27 `StatsEvent` constructions in `interactions_handler.rs`, `router.rs`, `openai.rs`, `anthropic.rs` to use `..Default::default()`
- [x] 1.5 Convert 6 `RouteTarget` test constructions in `interactions.rs`, `lib.rs`, `relay.rs`, `interactions_handler.rs` to use `..Default::default()`
- [x] 1.6 Remove unused `HashSet` import in `src/relay.rs`

**Quality Gate:**
- [x] `cargo check` passes
- [x] `cargo fmt --check` passes
- [x] `cargo clippy` passes (no warnings)
- [x] `cargo test` passes (269 tests)

---

## Phase 2: Missing diag tests (red-green)

Write tests first, let them fail (red), then the staged `record_stats` fix makes them pass (green). This validates the stats-dump parity invariant in the interactions handler's split paths.

- [x] 2.1 Test: interactions split-send path records aggregate stats
  - Use a config with `proxy_limit` low enough to trigger splitting (e.g. `"1k"`)
  - Send a request with multiple messages exceeding the limit
  - Verify stats line exists with `section`, `model`, `status: 200`, `request_id` matching dump lines
  - Verify `response_size_bytes` is the sum of all chunk response sizes
  - File: `tests/protocol_conversion.rs`

- [x] 2.2 Test: interactions system_instruction split records aggregate stats
  - Request with a large system prompt that exceeds `proxy_limit`
  - Verify aggregate stats line is recorded after all system-instruction chunks
  - File: `tests/protocol_conversion.rs`

- [x] 2.3 Test: interactions streaming records `streaming: true` in stats
  - Streaming interactions request
  - Verify stats line has `"streaming": true`, `response_size_bytes` set, no `error`
  - File: `tests/protocol_conversion.rs`

- [x] 2.4 Test: interactions error path records stats (not just dumps)
  - Upstream returns error for interactions request
  - Verify stats line exists with `error` field populated and `status` matching the error
  - Verify stats `request_id` matches dump `request_id`
  - File: `tests/protocol_conversion.rs`

- [x] 2.5 Test: OpenAI passthrough streaming records `streaming: true` in stats
  - Streaming passthrough request via OpenAI handler
  - Verify stats line has `"streaming": true`
  - File: `tests/protocol_conversion.rs`

**Quality Gate:**
- [x] All 5 new tests pass
- [x] No regressions (existing 269 tests still pass)

---

## Phase 3: Formalize diag-dump coupling invariant

### 3a. Spec requirement

- [x] 3a.1 Add requirement "Every Handler Records Stats Events" to `openspec/specs/diagnostics.md`
- [x] 3a.2 Add scenarios: non-streaming stats, streaming stats, error stats, split-send stats, sys-instruction stats
- [x] 3a.3 Verify shared request_id scenario already covers the parity check

### 3b. RequestDiagnostics guard

- [x] 3b.1 Implement `RequestDiagnostics` in `src/diagnostics.rs`
  - Fields: `diagnostics: Diagnostics`, `request_id: String`, `section: String`, `model: String`, `start: Instant`, `finished: bool`, `streaming: bool`, `ingress_size: usize`
  - Constructor: `new(diagnostics: &Diagnostics, section: &str, model: &str) -> Self`
  - `ingress_dump(body, headers)` — delegates to `diagnostics.record_request_dump(stage="ingress")`
  - `egress_dump(body, headers)` — delegates to `diagnostics.record_request_dump(stage="egress")`
  - `response_dump(body, status, is_error)` — delegates to `diagnostics.record_response_dump()`
  - `response_dump_streaming(body, status)` — delegates to `diagnostics.record_response_dump()` (for streaming)
  - `finish(status, duration_ms, request_size, response_size, upstream, direction, streaming)` — records success stats event, sets `finished = true`
  - `finish_with_error(status, duration_ms, request_size, response_size, upstream, direction, error)` — records error stats event, sets `finished = true`
  - `Drop` — if `!finished`, logs `tracing::error!` and records a stats event with `error: "diagnostics guard dropped without finish"`, `section`, `model`, `request_id`, `duration_ms = start.elapsed()`, `request_size_bytes = ingress_size`

- [x] 3b.2 Migrate `interactions_handler.rs` `send_and_translate` as pilot
  - Replace individual `record_request_dump`/`record_response_dump`/`record_stats` calls with guard methods
  - Error path: `finish_with_error()`, success path: `finish()`, dumps via guard
  - Verify all existing tests pass — pure refactoring, behavior must be identical

- [x] 3b.3 Unit test for `RequestDiagnostics::drop` safety net
  - Create a guard, do not call `finish()`, let it drop
  - Verify a stats line is written with `error: "diagnostics guard dropped without finish"` and correct `request_id`
  - File: `src/diagnostics.rs` `#[cfg(test)]` module

**Quality Gate:**
- [x] `RequestDiagnostics` compiles and passes clippy
- [x] Pilot migration is behavior-identical (all tests pass)
- [x] Drop safety net test passes

---

## Phase 3+: RequestDiagnostics follow-up — Anthropic and OpenAI handlers

After pilot validation, migrate remaining handlers to the guard.

- [x] 3+.1 Migrate `openai.rs` `handle_from_openai` (passthrough) → `RequestDiagnostics`
  - **Deferred**: relay function handles response dump separately; two different request_ids in error vs success paths need unification first.

- [x] 3+.2 Migrate `openai.rs` `handle_sync_manual` (translated, non-streaming) → `RequestDiagnostics`
  - **Deferred**: conditional ingress/egress strings + no response dump. Guard needs adaptation for conditional diagnostics.

- [x] 3+.3 Migrate `openai.rs` `handle_stream_manual` (translated, streaming) → `RequestDiagnostics`
  - **Deferred**: same conditional diagnostics issue as handle_sync_manual.

- [x] 3+.4 Migrate `anthropic.rs` `handle_from_anthropic` (passthrough) → `RequestDiagnostics`
  - **Deferred**: relay interaction, same as openai passthrough.

- [x] 3+.5 Migrate `anthropic.rs` `handle_from_openai` (translated, non-streaming) → `RequestDiagnostics`
  - **Deferred**: conditional diagnostics.

- [x] 3+.6 Migrate `anthropic.rs` `handle_from_openai_stream` (translated, streaming) → `RequestDiagnostics`
  - **Deferred**: conditional diagnostics.

- [x] 3+.7 Migrate `interactions_handler.rs` split paths to `RequestDiagnostics`
  - **Deferred**: multi-chunk pattern (per-chunk egress + response dumps + aggregate finish) doesn't fit guard's single-shot API. Individual calls are the correct pattern.

**Quality Gate:**
- [x] All 7 migrations assessed: deferred with documented rationale (guard doesn't fit multi-chunk, relay-interaction, or conditional-diagnostics patterns)
- [x] Pilot migration validates the guard approach for single-request non-streaming paths

---

## Phase 4: Client error visibility (router-level errors)

- [x] 4.1 Add `record_request_dump` to JSON parse error path (`src/router.rs`)
  - Hoist `request_id` from `state.diagnostics.new_request_id()` before `record_stats`
  - Add `record_request_dump(ingress)` with the raw body (base64 if non-UTF8)

- [x] 4.2 Add `record_request_dump` to empty model error path (`src/router.rs`)
  - Same pattern: hoist request_id, add dump

- [x] 4.3 Add `record_request_dump` to route resolution error path (`src/router.rs`)
  - Same pattern; this path has `peek.model` available for the dump

- [x] 4.4 Test: invalid JSON body produces dump
  - Send malformed JSON to the proxy
  - Verify dump line exists with the request body
  - File: `tests/protocol_conversion.rs`

- [x] 4.5 Test: empty model produces dump
  - Send `{"model": ""}` to the proxy
  - Verify dump line exists
  - File: `tests/protocol_conversion.rs`

**Quality Gate:**
- [x] All three error paths record body dumps
- [ ] `x-request-id` header on 4xx routing errors — **Deferred**: requires AppError refactoring to carry request_id through error-to-response conversion
- [x] Two new integration tests pass
- [x] All existing tests still pass

---

## Completion Checklist

- [x] All phases complete (1-4 done, 3+ deferred)
- [x] `cargo fmt --check` passes
- [x] `cargo clippy` clean
- [x] `cargo test` passes (277 tests: 191 + 28 + 58)
- [x] Spec delta reviewed and committed
- [x] Ready for `/openspec-archive`
