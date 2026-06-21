# Implementation Tasks: Fix Interactions Protocol Bugs

**Change ID:** `fix-interactions-protocol-bugs`

---

## Phase 1: Fix SSE deserialization (build.rs schema patch)

- [x] 1.1 Add schema patching in `build.rs` to remove `created`, `updated`, `steps` from `Interaction.required`
- [x] 1.2 Audit all `Interaction.{created,updated,steps}` access sites for Option handling
- [x] 1.3 Add test: `interaction.created` SSE event with incomplete interaction deserializes

**Quality Gate:**
- [x] `cargo check` passes
- [x] No `missing field 'created'` in tests

---

## Phase 2: Fix diagnostics guard early-return in streaming

- [x] 2.1 Add `guard.finish()` before each early `return` in `handle_stream_response` spawned task
- [x] 2.2 Choose appropriate status code and duration for the error paths
- [x] 2.3 Add test: guard is finished on tx send failure / stream error

**Quality Gate:**
- [x] No `diagnostics guard dropped without finish` when client disconnects
- [x] `cargo clippy` passes

---

## Phase 3: Forward tool definitions to interactions API

- [x] 3.1 Add `extract_anthropic_tools(body)` helper in `interactions.rs`
- [x] 3.2 Add `extract_openai_tools(body)` helper in `interactions.rs`
- [x] 3.3 Thread `tools` and `tool_choice` through `build_request_body()` → `CreateModelInteractionParams`
- [x] 3.4 Wire up extraction in `handle_from_anthropic` and `handle_from_openai`
- [x] 3.5 Add tests: tool definitions appear in outgoing request

**Quality Gate:**
- [x] Tools present in egress dump when ingress has tools
- [x] No tools field when ingress has no tools (no regression)

---

## Phase 4: Integration & Polish

- [x] 4.1 Run full test suite
- [x] 4.2 `cargo fmt --check`, `cargo clippy --locked -- -D warnings`
- [x] 4.3 `cargo test --locked`

**Quality Gate:**
- [x] All three checks pass
- [x] All existing tests pass (no regressions)

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Ready for `/openspec-archive`
