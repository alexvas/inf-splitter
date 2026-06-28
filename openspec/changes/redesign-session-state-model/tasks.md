# Implementation Tasks: Redesign Session State Model

**Change ID:** `redesign-session-state-model`

---

## Phase 1: Harness Filtering and Hashing

- [x] 1.1 RED — Anthropic filtering keeps harness messages only ✓ 2026-06-28
- [x] 1.2 RED — OpenAI filtering keeps system/developer/user/tool ✓ 2026-06-28
- [x] 1.3 RED — Control messages are stripped before hashing ✓ 2026-06-28
- [x] 1.4 GREEN — Implement filtering and canonical xxh3 hashing ✓ 2026-06-28
  - Added `xxhash-rust` to `Cargo.toml`
  - `filter_harness_messages` and `hash_harness_message` in `src/interactions.rs`

**Quality Gate:** PASSED — `cargo test --locked` all 399→418 tests pass

---

## Phase 2: InteractionStore and Frontier Selection

- [x] 2.1 RED — Insert and lookup upstream node by id ✓ 2026-06-28
- [x] 2.2 RED — Insert and lookup client node by id ✓ 2026-06-28
- [x] 2.3 RED — Hash index supports duplicate positions ✓ 2026-06-28
- [x] 2.4 RED — Walk client chain leaf to root ✓ 2026-06-28
- [x] 2.5 RED — Longest valid prefix ignores unrelated later hash ✓ 2026-06-28
- [x] 2.6 RED — Longest valid prefix returns previous client at boundary ✓ 2026-06-28
- [x] 2.7 RED — Frontier inside client node forks at parent ✓ 2026-06-28
- [x] 2.8 RED — Equal validated-chain tie-break deterministic ✓ 2026-06-28
- [x] 2.9 GREEN — Implement InteractionStore and frontier selection ✓ 2026-06-28
  - `ClientInteractionNode`, `UpstreamInteractionNode`, `ClientInteractionPosition`
  - `InteractionStore` with hash_index and upstream_to_clients
  - `find_frontier()` with longest valid prefix, fork, tie-break

**Quality Gate:** PASSED — all 11 Phase 2 tests pass

---

## Phase 3: Versioned Persistence and SessionInfo

- [x] 3.1 RED — V2 store round-trips sessions/interactions/in-flight ✓ 2026-06-28
- [x] 3.2 RED — Old v1 session file is ignored ✓ 2026-06-28
- [x] 3.3 RED — SessionInfo does not drive frontier ✓ 2026-06-28
- [x] 3.4 GREEN — Replace old SessionState store ✓ 2026-06-28
  - `SessionInfo` replaces `SessionState` for metadata (old store kept for backward compat)
  - `StoreDocumentV2` with version=2, sessions, interactions.clients, interactions.upstreams, in_flight
  - `StoreV2::load_from_disk` rebuilds hash_index and upstream_to_clients
  - `StoreV2::save_to_disk` atomic rename
  - Old v1 files detected and ignored with warning
  - Old `compute_delta`, `pending_sessions`, `clear_pending` kept for backward compat during transition

**Quality Gate:** PASSED — all 3 Phase 3 tests pass

---

## Phase 4: InFlightStore State Machine

- [x] 4.1 RED — Piece state transitions persist ✓ 2026-06-28
- [x] 4.2 RED — Complete batch inserts upstream nodes and one client node ✓ 2026-06-28
- [x] 4.3 RED — Failed piece cancels ACKed pieces, no client node ✓ 2026-06-28
- [x] 4.4 RED — Retry reuses matching in-flight batch ✓ 2026-06-28
- [x] 4.5 GREEN — Implement InFlightStore ✓ 2026-06-28
  - `create_batch`, `mark_response_started`, `mark_sent`, `ack_piece`
  - `fail_batch`, `complete_batch`, `remove_batch`
  - `find_matching_batch` by session_id + prev_id + message_hashes
  - `content_hash` reserved for future use

**Quality Gate:** PASSED — all 5 Phase 4 tests pass

---

## Phase 5: Handler Frontier Integration

- [x] 5.1 RED — Anthropic continuation uses hash frontier ✓ 2026-06-28
- [x] 5.2 RED — Anthropic rewrite with same count does not reuse stale id ✓ 2026-06-28
- [x] 5.3 RED — OpenAI assistant messages ignored for frontier ✓ 2026-06-28
- [x] 5.4 RED — All-known request fetches existing interaction from upstream ✓ 2026-06-28
- [x] 5.5 RED — All-known with multiple upstream_ids fetches and merges all pieces ✓ 2026-06-28
- [x] 5.6 RED — First-interaction fields follow frontier ✓ 2026-06-28
- [x] 5.7 GREEN — Replace `compute_delta` in `handle_from_anthropic` and `handle_from_openai` ✓ 2026-06-28
  - Strip controls, filter/hash harness messages, find frontier
  - Build request from unknown messages only
  - `replay_from_client_node` with multi-upstream merge via `fetch_upstream_interaction`
  - v2 store updated after successful interaction (ClientInteractionNode + UpstreamInteractionNode inserted)
  - Old `SessionStore` kept for split-send backward compat during transition
  - Harness frontier index mapped to full message array index for correct `start_index`
  - E2e tests updated for hash-based semantics (same message content required across turns)

**Quality Gate:** PASSED — 418 tests, all 27 e2e interactions tests pass

---

## Phase 6: Split-Send Preservation and State Rewrite

- [x] 6.1 RED — Full-body proxy_limit packing preserved ✓ 2026-06-28
- [x] 6.2 RED — System instruction split still precedes content ✓ 2026-06-28
- [x] 6.3 RED — Split-send creates upstream nodes and client node ✓ 2026-06-28
- [x] 6.4 RED — Non-streaming split merges all piece responses ✓ 2026-06-28
- [x] 6.5 RED — Non-streaming merge preserves tool calls across pieces ✓ 2026-06-28
- [x] 6.6 RED — Split-send failure cancels and records failed batch ✓ 2026-06-28
- [x] 6.7 GREEN — Rewrite `handle_split_send` around InFlightStore ✓ 2026-06-28
  - `handle_split_send` now creates InFlightBatch, tracks piece status (Pending→ResponseStarted→Sent→Acked)
  - Persists after every state transition via `save_to_disk()`
  - On completion: `complete_batch()` inserts UpstreamInteractionNodes + ClientInteractionNode
  - On failure: `fail_batch()` + cancels ACKed pieces upstream
  - Non-streaming responses merged via `merge_interaction_responses()`
  - Old per-chunk `message_count` updates removed; old SessionStore kept for backward compat
  - Added `set_piece_body()` to StoreV2 for updating piece request bodies before send

**Quality Gate:** PASSED — `cargo fmt --check` clean, `cargo clippy --locked` 0 errors, `cargo test --locked` 426 tests pass

---

## Phase 7: Streaming Split-Send Buffer

- [x] 7.1 RED — MemSseBuffer push/substitute/drain ✓ 2026-06-28
  - 9 unit tests: push/drain, piece ordering, duplicate index append, substitute_id, no-match, overflow error, overflow no-mutate, empty drain, large content within limit
- [x] 7.2 RED — Buffer overflow fails safely ✓ 2026-06-28
- [x] 7.3 RED — Anthropic split streaming emits one coherent final-id stream ✓ 2026-06-28
- [x] 7.4 RED — OpenAI split streaming emits final-id chat chunks ✓ 2026-06-28
- [x] 7.5 GREEN — Implement buffered streaming split-send ✓ 2026-06-28
  - Added `SseBuffer` trait, `MemSseBuffer`, `SseBufferError` in `src/sse.rs`
  - Replaced streaming path in `handle_split_send` to merge all pieces before synthesizing SSE
  - Added `streaming_response_from_merged` for pre-built protocol responses

**Quality Gate:** PASSED — `cargo fmt --check` clean, `cargo clippy --locked` 0 errors, `cargo test` 437 tests pass

---

## Phase 8: Startup Cleanup and Control Messages

- [x] 8.1 RED — Startup rebuilds derived indexes from persisted interactions ✓ 2026-06-28
  - Test `startup_rebuilds_derived_indexes` verifies hash_index and upstream_to_clients rebuild
- [x] 8.2 RED — Startup discards stale in-flight batches ✓ 2026-06-28
  - Test `startup_discards_non_fully_acked_batches` — fully-acked completes, Pending/Sent/Failed discarded
  - Test `startup_cleanup_does_not_touch_committed_interactions` — committed nodes survive
- [x] 8.3 RED — Clean-all clears all new stores ✓ 2026-06-28
  - Test `clean_all_clears_v2_stores` verifies sessions, interactions, hash_index, upstream_to_clients, in_flight all cleared
- [x] 8.4 RED — Extend-lifetime updates metadata and current interaction node ✓ 2026-06-28
  - Test `extend_lifetime_updates_v2_session_and_client_node` verifies SessionInfo and client node last_seen_utc updates
- [x] 8.5 GREEN — Replace old pending startup recovery with v2 in-flight batch cleanup ✓ 2026-06-28
  - Complete fully-acked batches, discard all other in-flight state (no re-fetch, no resend, no probe)
  - Added `discard_all_inflight()` to `StoreV2` — clears in-flight without completing
  - Added `clean_all()` to `StoreV2` — clears all stores
  - Added `clean_all()` to `StoreV2` — clears all stores
  - Added `extend_lifetime()` to `StoreV2` — updates SessionInfo + client node last_seen_utc
  - Added `all_upstream_ids()` to `StoreV2` — collects all upstream ids for cancellation
  - Updated `handle_control_action(CleanAll)` to also clean v2 stores and cancel v2 upstream interactions
  - Updated `handle_control_action(ExtendLifetime)` to also extend v2 session lifetime

**Quality Gate:** PASSED — `cargo fmt --check` clean, `cargo clippy --locked` 0 errors, `cargo test` 441 tests pass

---

## Phase 9: Regression and Final Checks

- [x] 9.1 Existing session response headers remain unchanged ✓ 2026-06-28
  - `x-claude-code-session-id` for Anthropic, `x-request-id` for OpenAI — covered by e2e tests
- [x] 9.2 Existing interactions response translation remains unchanged ✓ 2026-06-28
  - 27 e2e interactions tests pass, including streaming/non-streaming Anthropic and OpenAI
- [x] 9.3 Existing split-send error diagnostics remain covered ✓ 2026-06-28
  - `split_send_piece_failure_cancels_acked_pieces`, upstream error dumps, validation error dumps all pass

**Final Quality Gate:**
- [x] `cargo fmt --check` — clean
- [x] `cargo clippy --locked` — 0 errors (9 pre-existing warnings, none from Phase 7/8)
- [x] `cargo test --locked` — 441 tests pass

---

## Completion Checklist

- [x] Phase 1: Harness Filtering and Hashing ✓ 2026-06-28
- [x] Phase 2: InteractionStore and Frontier Selection ✓ 2026-06-28
- [x] Phase 3: Versioned Persistence and SessionInfo ✓ 2026-06-28
- [x] Phase 4: InFlightStore State Machine ✓ 2026-06-28
- [x] Phase 5: Handler Frontier Integration ✓ 2026-06-28
- [x] Phase 6: Split-Send Preservation and State Rewrite ✓ 2026-06-28
- [x] Phase 7: Streaming Split-Send Buffer ✓ 2026-06-28
- [x] Phase 8: Startup Cleanup and Control Messages ✓ 2026-06-28
- [x] Phase 9: Regression and Final Checks ✓ 2026-06-28
- [ ] Source specs updated by `/openspec-archive`
- [ ] Ready for merge
