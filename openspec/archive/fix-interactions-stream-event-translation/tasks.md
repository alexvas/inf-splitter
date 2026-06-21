# Implementation Tasks: Fix Interactions Stream Event Translation

**Change ID:** `fix-interactions-stream-event-translation`

---

## Phase 1: Fix build.rs code generation (RED)

- [x] 1.1 Add `try_const_tag()` helper in `build.rs`
- [x] 1.2 Wire it into `resolve_schema`
- [x] 1.3 Rebuild to regenerate `interactions_types.rs`
- [x] 1.4 Verify generated `InteractionSseEvent` has correct `serde(rename)` values: `"interaction.created"`, `"interaction.status_update"`, `"interaction.completed"`, `"error"`, `"step.start"`, `"step.delta"`, `"step.stop"`
- [x] 1.5 Verified via cargo build warnings

**Quality Gate:** PASSED (cargo check passes, correct rename values in generated code)

---

## Phase 2: Rewrite translate_stream_event (RED → GREEN)

- [x] 2.1 Replace manual `peek.get("event_type")` with `serde_json::from_str::<InteractionSseEvent>(data)`
- [x] 2.2 Implement `InteractionCreatedEvent` arm: emit `message_start` + `content_block_start`
- [x] 2.3 Implement `StepStart` arm: emit `content_block_start` (text type for all step types)
- [x] 2.4 Implement `StepDelta` arm: `TextDelta` → `text_delta`, `ThoughtSignatureDelta` → `signature_delta`; unhandled delta types → `tracing::warn!`
- [x] 2.5 Implement `StepStop` arm: emit `ContentBlockStop`
- [x] 2.6 Implement `InteractionCompletedEvent` arm: emit `MessageDelta` + `MessageStop`
- [x] 2.7 Implement `InteractionStatusUpdate` arm: return `None` with comment
- [x] 2.8 Implement `ErrorEvent` arm: map `error.code` → Anthropic `error.type`, `error.message` → Anthropic `error.message`
- [x] 2.9 Handle deserialization failure: log via `tracing::info!` with raw data prefix
- [x] 2.10 Update interaction_id tracking in `handle_stream_response` to use `InteractionSseEvent` deserialization
- [x] 2.11 Updated existing tests with correct event_type values
- [x] 2.12 step.start (thought) → content_block_start
- [x] 2.13 step.start (model_output) → content_block_start
- [x] 2.14 step.delta (text) → content_block_delta
- [x] 2.15 step.delta (thought_signature) → content_block_delta
- [x] 2.16 step.stop → content_block_stop
- [x] 2.17 error_event → StreamEvent::Error
- [x] 2.18 Integration test: full event sequence from dump

**Quality Gate:** PASSED (190 unit + 28 e2e + 51 integration = 269 tests pass)

---

## Phase 3: Verify and clean up

- [x] 3.1 `cargo fmt --check` — PASS
- [x] 3.2 `cargo clippy --locked -- -D warnings` — PASS
- [x] 3.3 `cargo test --locked` — 269/269 PASS
- [x] 3.4 Updated e2e tests to use new event_type values

**Quality Gate:** ALL PASSED

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Archived 2026-06-21
