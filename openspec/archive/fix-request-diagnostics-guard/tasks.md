# Implementation Tasks: Fix RequestDiagnostics Guard

**Change ID:** `fix-request-diagnostics-guard`

---

## Phase 1: Refactor RequestDiagnostics

- [x] 1.1 Change `finished: bool` to `finished: Cell<bool>`, `ingress_size: usize` to `ingress_size: Cell<usize>`
- [x] 1.2 Add optional stats detail fields: `input_messages: Cell<Option<usize>>`, `max_tokens: Cell<Option<u32>>`, `messages_detail_ingress: RefCell<Option<Value>>`, `messages_detail_egress: RefCell<Option<Value>>`
- [x] 1.3 Add setters: `set_input_messages`, `set_max_tokens`, `set_messages_detail_ingress`, `set_messages_detail_egress`
- [x] 1.4 Change `finish`/`finish_with_error` from `fn(mut self, ...)` to `fn(&self, ...)` — idempotent via `Cell<bool>`
- [x] 1.5 Update `finish`/`finish_with_error` to read optional detail fields and include them in `StatsEvent`
- [x] 1.6 Update `Drop` to use `Cell<bool>` (`.get()` instead of field access)
- [x] 1.7 Update `ingress_dump` to use `Cell<usize>` for `ingress_size`
- [x] 1.8 Update `disarm` to use `Cell<bool>` — still takes `self` (returns `request_id` String)
- [x] 1.9 Add unit test: idempotent `finish` (call twice, second is no-op)
- [x] 1.10 Verify Drop safety net test still passes

**Quality Gate:**
- [x] `cargo test` — unit tests pass
- [x] `cargo clippy` clean

**Note:** Phase 1 was already complete when the change was picked up. `Cell` fields were subsequently upgraded to `Mutex` for `Sync`-ness (so `&RequestDiagnostics` is `Send`). Deferred dump recording was added so ingress/egress dumps are recorded with the correct `is_error` flag in `finish`/`finish_with_error`.

---

## Phase 2: Update pilot + migrate interactions split paths

- [x] 2.1 Update `send_and_translate` for new API (minor: `guard.ingress_dump` no longer needs `&mut`)
- [x] 2.2 Migrate `handle_split_send` to `RequestDiagnostics`
  - Create guard at start; ingress dump via guard; per-chunk egress + response dumps; finish/finish_with_error
- [x] 2.3 Migrate `send_split_system_instruction` to `RequestDiagnostics`
  - Same pattern as handle_split_send

**Quality Gate:**
- [x] All existing interactions tests pass
- [x] No behavior change

---

## Phase 3: Migrate OpenAI handlers

- [x] 3.1 Migrate `handle_from_openai` (passthrough, relay-interaction)
  - Create guard once; conditional egress dump; same request_id for error + success; finish after relay
- [x] 3.2 Migrate `handle_sync_manual` (translation, non-streaming, conditional diagnostics)
  - Guard with detail setters for `input_messages`/`max_tokens`/`messages_detail_*`; conditional ingress/egress
- [x] 3.3 Migrate `handle_stream_manual` (translation, streaming, conditional diagnostics)
  - Same as handle_sync_manual but streaming=true, disarm for stream task

**Quality Gate:**
- [x] All existing OpenAI handler tests pass
- [x] `cargo test` — 279 tests pass

---

## Phase 4: Migrate Anthropic handlers

- [x] 4.1 Migrate `handle_from_anthropic` (passthrough, relay-interaction)
  - Same pattern as openai passthrough
- [x] 4.2 Migrate `handle_from_openai` (translation, non-streaming, conditional diagnostics)
  - Guard with detail setters; conditional ingress/egress
- [x] 4.3 Migrate `handle_from_openai_stream` (translation, streaming, conditional diagnostics)
  - Guard with detail setters; disarm for stream task

**Quality Gate:**
- [x] All existing Anthropic handler tests pass
- [x] `cargo test` — all tests pass

---

## Phase 5: Add integration tests for migrated handlers

- [x] 5.1 Test: openai passthrough stats share request_id between error and success paths
- [x] 5.2 Test: openai translation handler records input_messages + max_tokens in stats
- [x] 5.3 Test: anthropic passthrough stats share request_id with dump

**Quality Gate:**
- [x] All new tests pass
- [x] No regressions

---

## Completion Checklist

- [x] All phases complete
- [x] `cargo fmt --check` passes
- [x] `cargo clippy` clean
- [x] `cargo test` passes (279 tests)
- [x] Spec delta reviewed and committed
- [x] Ready for `/openspec-archive`
