# Implementation Tasks: Fix Interactions Protocol Correctness Bugs

**Change ID:** `fix-interactions-protocol-correctness`
**Approach:** Red-Green (TDD)
**Completed:** 2026-06-24

---

## Phase 1: URL & Auth

- [x] 1.1 **RED** — Test: `build_interaction_url` with query params in endpoint, verify lifecycle URL preserves `?key=ABC`
- [x] 1.2 **GREEN** — Fix `build_interaction_url` to parse and reattach query string from `endpoint_interactions`

**Quality Gate:** PASSED — all 7 tests pass

---

## Phase 2: Token Handling

- [x] 2.1 **RED** — Test: `build_request_body` with `client_max_tokens=100`, `route.max_tokens=1000` → expect `max_output_tokens=100` (client wins)
- [x] 2.2 **RED** — Test: `build_request_body` with `client_max_tokens=1000`, `route.max_tokens=100` → expect `max_output_tokens=100` (route caps)
- [x] 2.3 **RED** — Test: `build_request_body` with `client_max_tokens=500`, no route limit → expect `500`
- [x] 2.4 **GREEN** — Fix `build_request_body` to use `min(client, route)` semantics
- [x] 2.5 **RED** — Test: OpenAI ingress with only `max_completion_tokens=200` → `ingress_max_tokens=Some(200)`
- [x] 2.6 **RED** — Test: OpenAI ingress with both `max_completion_tokens=200` and `max_tokens=100` → `max_completion_tokens` wins
- [x] 2.7 **GREEN** — Read `max_completion_tokens` in `handle_from_openai`, fallback to `max_tokens`
- [x] 2.8 **RED** — Test: `total_input_tokens = 5_000_000_000i64` → translated to `u32::MAX` with warning
- [x] 2.9 **GREEN** — Replace `as u32` with `clamp_i64_to_u32` + `tracing::warn!`

**Quality Gate:** PASSED — all 33 tests pass

---

## Phase 3: Session State Correctness

- [x] 3.1 **RED** — Context reset: session has `message_count=5`, client sends 2 messages → `previous_interaction_id` must be `None`
- [x] 3.2 **GREEN** — Clear `previous_interaction_id` when `start_index == 0` (both Anthropic and OpenAI paths)
- [x] 3.3 **RED** — Streaming `InteractionCreatedEvent` arrives, client disconnects before EOF → session has valid `interaction_id`
- [x] 3.4 **GREEN** — Update session `interaction_id` eagerly from `InteractionCreatedEvent` in stream task
- [x] 3.5 **RED** — `compute_delta(5, 5)` with `previous_interaction_id = Some("int-1")` → handler fetches existing interaction
- [x] 3.6 **GREEN** — Handle empty delta + `Some(prev_id)` by fetching existing interaction via GET (`replay_interaction`)
- [x] 3.7 **RED** — 5 ingress messages produce 4 Content items → `message_count` uses proportion, not Content count
- [x] 3.8 **GREEN** — Track ingress message proportion for session updates in `handle_split_send`
- [x] 3.9 **RED** — `handle_split_send` translation fails after all chunks succeed → session remains `pending=true`
- [x] 3.10 **GREEN** — Move session finalization AFTER successful response translation

**Quality Gate:** PASSED — all 280 lib tests pass

---

## Phase 4: Streaming & SSE Correctness

- [x] 4.1 **RED** — `InteractionCompletedEvent` after `StepStop(index=0)` → emits `message_delta` + `message_stop`, NO duplicate `content_block_stop`
- [x] 4.2 **GREEN** — Track last active block index; `StepStop` clears it; `InteractionCompletedEvent` skips stop when None

**Quality Gate:** PASSED — all 20 translate tests pass

---

## Phase 5: Split-Send Correctness

- [x] 5.1 **RED** — `proxy_limit=100KB`, envelope=60KB, system_instruction=50KB → split uses `limit - envelope_overhead`
- [x] 5.2 **GREEN** — Compute envelope overhead, pass `sys_limit = limit.saturating_sub(envelope_without_sys)` to split
- [x] 5.3 **RED** — `send_split_system_instruction` receives malformed JSON from upstream → error propagates
- [x] 5.4 **GREEN** — Replace `if let Ok` with `serde_json::from_str(...).map_err(|e| guard.abort_internal(...))?`

**Quality Gate:** PASSED — all 280 lib tests pass

---

## Phase 6: OpenAI→Interactions Specific

- [x] 6.1 **RED** — `extract_openai_system` with `[{role:"user"}, {role:"system", content:"Be concise"}]` → returns `Some("Be concise")`
- [x] 6.2 **GREEN** — `iter().find()` instead of `first()` in `extract_openai_system`

**Quality Gate:** PASSED — all 3 extract_openai_system tests pass

---

## Phase 7: Integration & Polish

- [x] 7.1 Run full test suite (`cargo test`)
- [x] 7.2 Run clippy and fmt checks
- [x] 7.3 Verify no regressions in existing flow

**Quality Gate:**
- [x] All 375 tests pass (280 lib + 28 e2e + 67 protocol_conversion)
- [x] `cargo fmt --check` clean
- [x] `cargo clippy --locked` — 1 pre-existing warning, no new warnings
- [x] Ready for `/openspec-archive`
