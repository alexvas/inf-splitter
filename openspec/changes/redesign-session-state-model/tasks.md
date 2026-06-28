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

### 7.1 RED — MemSseBuffer push/substitute/drain
- Push raw upstream SSE bytes with intermediate ids and piece index
- Count buffered raw bytes toward 100 MB limit
- Substitute ids with final id
- Drain returns piece buffers in order with only final id references

### 7.2 RED — Buffer overflow fails safely
- Buffer exceeds 100 MB of raw upstream SSE bytes
- THEN error is returned, batch is failed, and ACKed pieces are cancelled best-effort/asynchronously

### 7.3 RED — Anthropic split streaming emits one coherent final-id stream
- P0 -> `int-A`, P1 -> `int-B`
- Client stream contains `int-B`, not `int-A`

### 7.4 RED — OpenAI split streaming emits final-id chat chunks
- Same upstream pieces
- OpenAI client receives chat-completion chunks derived after substitution

### 7.5 GREEN — Implement buffered streaming split-send
- Add `SseBuffer`/`MemSseBuffer` with `push(piece_index, bytes)`, `substitute_id(from, to)`, `drain()`, `len_bytes()`
- Buffer every piece SSE until final id known
- Substitute intermediate ids
- Translate/drain one client-visible stream

**Quality Gate:** `cargo test --locked` — Phase 7 tests pass

---

## Phase 8: Startup Recovery and Control Messages

### 8.1 RED — Startup rebuilds derived indexes from persisted interactions
- Persist nodes without runtime `hash_index` or `upstream_to_clients`
- Load store
- THEN hash lookup works
- AND reverse upstream lookup works

### 8.2 RED — Startup resumes pending in-flight piece
- Persist batch with P0 Acked and P1 Pending plus request data
- Startup resends P1 with previous id from P0

### 8.3 RED — Clean-all clears all new stores
- Sessions, interaction nodes, hash index, reverse upstream index, and in-flight batches exist
- Clean-all processed
- THEN referenced upstreams and reverse-index orphan upstreams are cancelled/deleted best-effort
- AND all local stores are empty after best-effort upstream cleanup

### 8.4 RED — Extend-lifetime updates metadata and current interaction node
- Current request matches known client node
- Extend-lifetime processed
- THEN SessionInfo and current interaction node last-seen metadata update

### 8.5 GREEN — Replace old pending startup recovery and update control actions
- Remove `pending_sessions` startup loop
- Load/recover v2 stores
- Resume/verify in-flight batches
- Update clean-all and extend-lifetime for new stores

**Quality Gate:** `cargo test --locked` — Phase 8 tests pass

---

## Phase 9: Regression and Final Checks

### 9.1 Existing session response headers remain unchanged
- Anthropic ingress returns `x-claude-code-session-id`
- OpenAI ingress returns `x-request-id`

### 9.2 Existing interactions response translation remains unchanged
- Non-streaming Anthropic/OpenAI responses match current behavior
- Streaming Anthropic/OpenAI responses match current behavior outside split-send buffering

### 9.3 Existing split-send error diagnostics remain covered
- Upstream errors still record diagnostics
- Request/response dumps still work for split and streaming paths

**Final Quality Gate:**
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --locked`
- [ ] `cargo test --locked`

---

## Completion Checklist

- [x] Phase 1: Harness Filtering and Hashing ✓ 2026-06-28
- [x] Phase 2: InteractionStore and Frontier Selection ✓ 2026-06-28
- [x] Phase 3: Versioned Persistence and SessionInfo ✓ 2026-06-28
- [x] Phase 4: InFlightStore State Machine ✓ 2026-06-28
- [x] Phase 5: Handler Frontier Integration ✓ 2026-06-28
- [x] Phase 6: Split-Send Preservation and State Rewrite ✓ 2026-06-28
- [ ] Phase 7: Streaming Split-Send Buffer
- [ ] Phase 8: Startup Recovery and Control Messages
- [ ] Phase 9: Regression and Final Checks
- [ ] Source specs updated by `/openspec-archive`
- [ ] Ready for merge
