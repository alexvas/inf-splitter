# Proposal: Fix RequestDiagnostics guard to support all handler patterns

**Change ID:** `fix-request-diagnostics-guard`
**Created:** 2026-06-21
**Status:** Implementation Complete

---

## Problem Statement

The `RequestDiagnostics` guard introduced in `add-default-and-diag-invariants` proved successful for the pilot migration (`send_and_translate`) but was deferred for 7 other handlers because three patterns could not be modelled. The deferred handlers also contain a related bug.

### Bug: different `request_id` in error vs success paths

In the passthrough handlers (`openai.rs` `handle_from_openai`, `anthropic.rs` `handle_from_anthropic`), the error path creates `request_id` at the point of failure, while the success path creates a **different** `request_id` later:

```rust
// openai.rs handle_from_openai (current code)
if is_err {
    let request_id = self.diagnostics.new_request_id();  // ID #1 — error
    self.diagnostics.record_stats(&StatsEvent { request_id: ..., error: Some(...), ... });
    self.diagnostics.record_request_dump(&request_id, ...);  // ingress/egress dumps
    return response...;
}
let request_id = self.diagnostics.new_request_id();  // ID #2 — different, success only
let relayed = relay_openai_upstream(..., RelayContext { request_id: request_id.clone(), ... })?;
self.diagnostics.record_stats(&StatsEvent { request_id: ..., error: None, ... });
```

This violates the spec requirement "All dump lines and the stats line for that request share the same `request_id`" because:
1. The error path's dump lines use ID #1 but the stats line uses ID #1 too — but they'll never be correlated with any success-path event
2. Two `new_request_id()` calls waste a counter value for every request
3. If a request fails, the error stats and error dump share an ID, but the operator has no way to know this was the same HTTP request that arrived (the ingress body was already consumed)

The fix is trivial: hoist `request_id` before the branch. `RequestDiagnostics` does this automatically since the guard creates it at construction.

### Deferred patterns from the previous guard

**1. Multi-chunk (split-send, sys-instruction split)** ...
Multiple egress dumps and response dumps per request, with a single aggregate stats event at the end. The guard's single-shot `egress_dump`/`response_dump`/`finish` model couldn't express "call dump N times, then finish once."

### 2. Relay-interaction (passthrough handlers)
The error path and success path create **different** `request_id` values (two `new_request_id()` calls). The guard creates one `request_id` at construction — structurally correct, but the relay function (`relay_openai_upstream`) handles the response dump internally with a `RelayContext` that needs the `request_id`. Additionally, egress dumps are conditional on `dump_enabled()`.

### 3. Conditional diagnostics (translation handlers)
`ingress_str`/`egress_str` are `Option<String>` (only populated when `dump_enabled()`). `messages_detail_ingress`/`messages_detail_egress` are `Option<Value>` (only when `stats_enabled()`). `input_messages`/`max_tokens` are computed from the request struct. The current guard has no API for any of these optional fields.

### Root cause

The guard has two design limitations:
1. **`finish`/`finish_with_error` consume `self`** — preventing any further use after stats recording. Multi-chunk needs to record response dumps and finish separately.
2. **No support for optional `StatsEvent` fields** — `input_messages`, `max_tokens`, `messages_detail_ingress`, `messages_detail_egress` cannot be set through the guard.

## Proposed Solution

Refactor `RequestDiagnostics` with two changes:

### Change 1: `&self` methods with `Cell<bool>` for idempotent finish

Replace `finished: bool` with `finished: Cell<bool>`. Change `finish`/`finish_with_error` from `fn(mut self, ...)` to `fn(&self, ...)`. The first call sets `finished` to `true`; subsequent calls are no-ops.

This enables:
- **Multi-chunk:** `guard.egress_dump()` + `guard.response_dump()` per chunk in a loop, then `guard.finish()` once after the loop
- **Relay-interaction:** Create guard once. Both error and success paths share the same `request_id`. Success path calls `finish()` after relay returns.

### Change 2: Add optional stats detail setters

```rust
guard.set_input_messages(3);
guard.set_max_tokens(4096);
guard.set_messages_detail_ingress(json!(...));
guard.set_messages_detail_egress(json!(...));
```

These populate `Cell<Option<usize>>`, `Cell<Option<u32>>`, `Mutex<Option<Value>>`, `Mutex<Option<Value>>` fields on the guard. `finish`/`finish_with_error` read them when building the `StatsEvent`.

### Change 3: `Mutex` instead of `RefCell` for `Send`-ness

Two fields use `Mutex<Option<Value>>` instead of `RefCell<Option<Value>>`:

```rust
messages_detail_ingress: Mutex<Option<serde_json::Value>>,
messages_detail_egress: Mutex<Option<serde_json::Value>>,
```

`Cell` types are already `Send`. This makes the entire guard `Send`, enabling it to cross `tokio::spawn` boundaries in streaming handlers. The streaming task moves the guard by value and calls `guard.response_dump()` + `guard.finish()` directly — no more `disarm()` + raw `diagnostics.record_*` workaround.

### Updated struct

```rust
pub struct RequestDiagnostics {
    diagnostics: Diagnostics,
    request_id: String,
    section: String,
    model: String,
    start: Instant,
    finished: Cell<bool>,
    ingress_size: Cell<usize>,
    input_messages: Cell<Option<usize>>,
    max_tokens: Cell<Option<u32>>,
    messages_detail_ingress: Mutex<Option<serde_json::Value>>,
    messages_detail_egress: Mutex<Option<serde_json::Value>>,
}
```

### How each deferred pattern works with the fix

**Multi-chunk:**
```rust
let guard = RequestDiagnostics::new(&self.diagnostics, &route.section, model);
guard.ingress_dump(ingress_body, request_headers);
for chunk in &chunks {
    guard.egress_dump(&chunk_body, request_headers);
    let upstream = send(chunk).await?;
    if !upstream.status().is_success() {
        guard.response_dump(&error_body, status, true);
        guard.finish_with_error(status, duration, ingress_len, Some(err_len), up, dir, false, err);
        return ...;
    }
    guard.response_dump(&response_text, 200, false);
}
guard.finish(200, total_duration, ingress_len, Some(total_bytes), up, dir, false);
```

**Relay-interaction:**
```rust
let guard = RequestDiagnostics::new(&self.diagnostics, &route.section, &model);
guard.ingress_dump(&original_body, request_headers);
if let Some(ref body_bytes) = downstream_body {
    guard.egress_dump(body_bytes, request_headers);
}
if is_err {
    guard.finish_with_error(status, duration, request_size, Some(err_len), endpoint, "openai->openai", false, error_body);
    return ...;
}
let relayed = relay_openai_upstream(upstream, &guard).await?;  // guard records response dump internally
guard.finish(relayed.status(), duration, request_size, None, endpoint, "openai->openai", is_streaming);
```

The relay function receives `&RequestDiagnostics` instead of `RelayContext`. It calls `guard.response_dump()` (non-streaming) or `guard.response_dump_streaming()` (streaming, via `DiagnosticStream`) directly — no more raw `record_response_dump` calls or `RelayContext` struct. This eliminates all four fields (`diagnostics`, `request_id`, `model`, `section`) that the guard already owns.

**Conditional diagnostics:**
```rust
let guard = RequestDiagnostics::new(&self.diagnostics, &route.section, &req.model);
if self.diagnostics.stats_enabled() {
    guard.set_input_messages(req.messages.len());
    guard.set_max_tokens(req.max_tokens);
    guard.set_messages_detail_ingress(anthropic_messages_detail(req));
    guard.set_messages_detail_egress(messages_detail_from_value(&prepared.value));
}
if let Some(ref s) = ingress_str {
    guard.ingress_dump(s.as_bytes(), request_headers);
}
if let Some(ref s) = egress_str {
    guard.egress_dump(s.as_bytes(), request_headers); // or empty headers
}
// ... no response dump (translation handlers don't record it)
if error {
    guard.finish_with_error(status, duration, ...);
} else {
    guard.finish(200, duration, ...);
}
```

## Scope

### In Scope
- Refactor `RequestDiagnostics`: `Cell<bool>` for finished, `&self` methods, optional stats detail setters
- **Bug fix:** unify `request_id` in passthrough handlers — hoist before error/success branch
- Migrate `send_and_translate` to updated API (verify no behavior change)
- Migrate `handle_split_send` + `send_split_system_instruction` (multi-chunk)
- Migrate `handle_from_openai` (openai passthrough, relay-interaction)
- Migrate `handle_sync_manual` + `handle_stream_manual` (openai translation, conditional diagnostics)
- Migrate `handle_from_anthropic` (anthropic passthrough, relay-interaction)
- Migrate `handle_from_openai` + `handle_from_openai_stream` (anthropic translation, conditional diagnostics)
- Unit test: Drop safety net still works with `Cell<bool>`
- Unit test: `finish` called twice is idempotent
- Integration test: passthrough error and success paths produce the same `request_id`

### Out of Scope
- Adding response dumps to translation handlers (separate concern — they currently don't record response dumps at all)
- `x-request-id` response header (deferred in previous change)

### Removals
- `RelayContext` struct in `src/relay.rs` — replaced by `&RequestDiagnostics` parameter to relay functions
- `DiagnosticStream` carrying its own `diagnostics`/`request_id`/`section`/`model` fields — replaced by `&RequestDiagnostics`

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/diagnostics.rs` | Refactor `RequestDiagnostics` | `Cell<bool>`, `&self` methods, detail setters, updated `Drop` |
| `src/relay.rs` | Remove `RelayContext`; update relay functions | Relay functions take `&RequestDiagnostics` instead of `RelayContext` |
| `src/interactions_handler.rs` | Update `send_and_translate`; migrate split paths | Minor API update + 2 new migrations |
| `src/openai.rs` | Migrate 3 handler functions + bug fix | `handle_from_openai` (unify request_id), `handle_sync_manual`, `handle_stream_manual` |
| `src/anthropic.rs` | Migrate 3 handler functions + bug fix | `handle_from_anthropic` (unify request_id), `handle_from_openai`, `handle_from_openai_stream` |
| `openspec/specs/diagnostics.md` | Delta | Update `RequestDiagnostics` guard spec |
| `tests/protocol_conversion.rs` | New tests | 4+ tests for migrated handlers |

## Success Criteria
- [ ] `RequestDiagnostics` refactored with `Cell<bool>`, `&self` methods, detail setters
- [ ] **Bug fixed:** passthrough handlers use a single `request_id` for both error and success paths
- [ ] All 10 handler functions use `RequestDiagnostics` (1 already + 9 new)
- [ ] No `record_request_dump`/`record_response_dump`/`record_stats` calls remain in handler code that the guard covers
- [ ] Drop safety net test still passes (with `Cell<bool>`)
- [ ] Idempotent `finish` test passes
- [ ] Integration test: passthrough error + success share `request_id`
- [ ] All existing 277 tests still pass
- [ ] `cargo fmt`, `cargo clippy`, `cargo test` all pass

---

## Archive Information

**Archived:** 2026-06-21
**Outcome:** Successfully implemented

### Implementation Notes

- Switched from `Cell` to `Mutex` for interior-mutable fields to make `RequestDiagnostics` `Send + Sync`, enabling `&guard` references across `.await` points
- Added deferred dump recording: ingress/egress dumps are stored in the guard and flushed in `finish`/`finish_with_error` with the correct `is_error` flag
- Added `headers` parameter to `response_dump()` so relay functions can pass response headers
- `RelayContext` struct removed from `src/relay.rs`

### Files Modified
- `src/diagnostics.rs` — Mutex fields, deferred dump storage, headers param, StoredDump type alias
- `src/relay.rs` — Removed RelayContext and DiagnosticStream::from_guard
- `src/openai.rs` — Migrated handle_from_openai, handle_sync_manual, handle_stream_manual
- `src/anthropic.rs` — Migrated handle_from_anthropic, handle_from_openai, handle_from_openai_stream
- `src/interactions_handler.rs` — Migrated send_and_translate, handle_stream_response, handle_split_send, send_split_system_instruction

### Specs Updated
- `openspec/specs/diagnostics.md` — Updated RequestDiagnostics Session Guard to v2

### Quality Verification
- `cargo fmt` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo test` — 279 tests pass
