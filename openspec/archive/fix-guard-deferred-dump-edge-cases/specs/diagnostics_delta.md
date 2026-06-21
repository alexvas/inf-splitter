# Delta: Diagnostics — Deferred-Dump Edge Case Fixes

**Change ID:** `fix-guard-deferred-dump-edge-cases`
**Affects:** `src/diagnostics.rs`, `src/openai.rs`, `src/anthropic.rs`, `src/interactions_handler.rs`

---

## MODIFIED

### Requirement: RequestDiagnostics Session Guard (v2)

**StoredDump extended** — now includes capture-time timestamp and optional status:

```rust
type StoredDump = (DumpBody, Vec<(String, String)>, String, Option<u16>);
//                 body      headers                ts     status
```

**`ingress_dump`** captures `ts_string()` at record time, stores `status: None`.

**`egress_dump`** captures `ts_string()` at record time, stores `status: None`.

**`flush_deferred_dumps`** uses the stored timestamp and status for each dump event instead of calling `ts_string()` at flush time and hardcoding `status: None`.

#### Scenario: Per-dump capture-time timestamps
- GIVEN a split-send with 2 chunks sent 5 seconds apart
- WHEN `guard.finish()` flushes deferred dumps
- THEN the first chunk's egress dump has a timestamp ~5s before the second chunk's
- AND both timestamps differ from the stats event timestamp

#### Scenario: Passthrough request dumps carry response status
- GIVEN an anthropic→anthropic passthrough success request
- WHEN the request completes with status 200
- THEN ingress and egress request dumps have `status: 200`

### Requirement: Every Protocol Handler Records Dump Events

#### Scenario: Split-send error preserves prior chunk dumps
- GIVEN a split-send with 3 chunks where chunk 2 fails
- WHEN `finish_with_error` is called
- THEN chunk 1's egress dump has `is_error: false` (it succeeded)
- AND chunk 2's egress dump has `is_error: true` (it failed)
- AND chunk 3 was never sent (no dump)

#### Scenario: Translation handler stats omit missing detail fields
- GIVEN a passthrough request body with no `messages` field
- WHEN stats are recorded
- THEN `messages_detail_ingress` is absent from the JSON output
- AND `messages_detail_egress` is absent from the JSON output

### Requirement: Dump Event Format

*(No format change — timestamps remain ISO 8601 UTC, status remains `Option<u16>`.)*

## ADDED

### Requirement: Cleanup — dead code and duplication removed

- `disarm()` method removed (zero callers)
- `_route` parameter removed from `handle_stream_response`
- `header_pairs_from_map()` helper extracted
- `mark_finished()` helper extracted

#### Scenario: No dead code warnings
- GIVEN the codebase after cleanup
- WHEN `cargo clippy` runs
- THEN no dead-code or unused-import warnings related to the removed items

## REMOVED

- `disarm()` method on `RequestDiagnostics` — superseded by moving the guard by value into spawned tasks.
- `_route: &RouteTarget` parameter on `handle_stream_response` — diagnostics data now comes from the guard.
