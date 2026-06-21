# Delta: Diagnostics — RequestDiagnostics Guard v2

**Change ID:** `fix-request-diagnostics-guard`
**Affects:** `src/diagnostics.rs`, `src/interactions_handler.rs`, `src/openai.rs`, `src/anthropic.rs`, `openspec/specs/diagnostics.md`

---

## ADDED

### Requirement: RequestDiagnostics supports all handler patterns

The `RequestDiagnostics` session guard binds stats and dump recording for a single request. It supports four handler patterns:

1. **Single-request** (non-streaming passthrough/conversion): one ingress dump, one egress dump, one response dump, one finish
2. **Multi-chunk** (split-send, sys-instruction split): one ingress dump, per-chunk egress + response dumps, one aggregate finish
3. **Relay-interaction** (passthrough handlers): guard creates `request_id`; relay function handles response dump internally; guard finishes after relay
4. **Conditional diagnostics** (translation handlers): dumps only when `dump_enabled()`, stats detail fields only when `stats_enabled()`

#### `Cell<bool>` for idempotent finish

`finish()` and `finish_with_error()` take `&self` and are idempotent. The first call sets the internal `finished` flag; subsequent calls are no-ops. This enables per-chunk error handling followed by early return — the guard records stats on the first error, and subsequent drops are harmless.

#### Optional stats detail setters

```rust
guard.set_input_messages(n);
guard.set_max_tokens(n);
guard.set_messages_detail_ingress(value);
guard.set_messages_detail_egress(value);
```

These populate the corresponding `StatsEvent` fields. If never called, the fields are `None` (omitted from serialization).

#### Scenario: Multi-chunk split-send
- GIVEN a request whose content exceeds `proxy_limit`
- WHEN the content is split into N chunks and sent sequentially
- THEN the guard records one ingress dump, N egress dumps, N response dumps
- AND `guard.finish()` records one aggregate stats event with `response_size_bytes` = sum of all chunk responses
- AND all events share the same `request_id`

#### Scenario: Per-chunk error with early return
- GIVEN a split-send where chunk 2 of 5 fails
- WHEN the upstream returns an error for chunk 2
- THEN `guard.finish_with_error()` records an error stats event
- AND the function returns early
- AND subsequent `guard.drop()` is a no-op (already finished)

#### Scenario: Relay-interaction with shared request_id
- GIVEN a passthrough handler using `RequestDiagnostics`
- WHEN the error path records stats via `guard.finish_with_error()`
- OR the success path relays through `relay_*_upstream` and records stats via `guard.finish()`
- THEN both paths use the same `request_id` (created once at guard construction)
- AND the relay function uses `guard.request_id()` for its response dump

#### Scenario: Conditional diagnostics
- GIVEN a translation handler where `dump_enabled()` is false
- WHEN the handler uses `RequestDiagnostics`
- THEN `ingress_dump`/`egress_dump` are never called (body strings are `None`)
- AND `set_input_messages`/`set_max_tokens`/`set_messages_detail_*` are only called when `stats_enabled()` is true
- AND `finish()` still records a stats event with the fields that were set

## MODIFIED

### Requirement: RequestDiagnostics Session Guard

*(Updated from previous version — `finish`/`finish_with_error` now take `&self` instead of `self`; `finished` uses `Cell<bool>` for interior mutability; optional stats detail fields added.)*

The guard struct:

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

**Methods unchanged:** `new`, `request_id`, `ingress_dump`, `egress_dump`, `response_dump`, `response_dump_streaming`, `Drop`

**Methods changed:** `finish(&self, ...)`, `finish_with_error(&self, ...)` — now take `&self`, idempotent via `Cell<bool>`

**Methods removed:** `disarm()` — no longer needed. `Mutex` makes the guard `Send`; streaming tasks move it by value. Relay functions receive `&guard`.

**Methods added:** `set_input_messages`, `set_max_tokens`, `set_messages_detail_ingress`, `set_messages_detail_egress`

## REMOVED

(None)
