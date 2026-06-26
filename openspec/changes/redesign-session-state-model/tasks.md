# Implementation Tasks: Redesign Session State Model

**Change ID:** `redesign-session-state-model`

---

## Phase 1: Harness Filtering and Hashing

### 1.1 RED — Anthropic filtering keeps harness messages only
- GIVEN Anthropic `[{role: "user"}, {role: "assistant"}, {role: "user"}]`
- THEN `filter_harness_messages` returns the two `user` messages

### 1.2 RED — OpenAI filtering keeps system/developer/user/tool
- GIVEN OpenAI `[{role: "system"}, {role: "developer"}, {role: "user"}, {role: "assistant"}, {role: "tool"}]`
- THEN assistant is dropped and the other four roles remain in order

### 1.3 RED — Control messages are stripped before hashing
- GIVEN messages include one control sentinel and one user message
- THEN only the user message hash participates in frontier selection

### 1.4 GREEN — Implement filtering and canonical xxh3 hashing
- Add `xxhash-rust` to `Cargo.toml`/lockfile
- Hash `serde_json::to_vec(Value)` after control stripping

**Quality Gate:** `cargo test --locked` — Phase 1 tests pass

---

## Phase 2: InteractionStore and Frontier Selection

### 2.1 RED — Insert and lookup node by id
- Insert `InteractionNode { id: "int-1", prev_id: None, message_hashes: vec![0xA] }`
- THEN `get("int-1")` succeeds

### 2.2 RED — Hash index supports duplicate positions
- Insert branch A and branch B both containing `0xA`
- THEN `lookup_hash(0xA)` returns both positions

### 2.3 RED — Walk chain leaf to root
- GIVEN `int-1 -> int-2 -> int-3`
- THEN `walk_chain("int-3")` returns `[int-3, int-2, int-1]`

### 2.4 RED — Longest valid prefix ignores unrelated later hash
- Store contains `0xB` only on unrelated branch
- Incoming hashes `[0xA, 0xB]`
- THEN frontier is `0`, not `2`

### 2.5 RED — Longest valid prefix returns previous interaction
- Known chain hashes `[0xA, 0xB]` ending at `int-2`
- Incoming `[0xA, 0xB, 0xC]`
- THEN frontier is `2`, previous interaction is `int-2`

### 2.6 RED — Duplicate branch tie-break deterministic
- Two chains match same prefix and same `last_seen_utc`
- THEN lexicographically smallest terminal id wins

### 2.7 GREEN — Implement InteractionStore and frontier selection
- `HashMap<String, InteractionNode>`
- `HashMap<u64, Vec<InteractionPosition>>`
- `find_frontier(hashes) -> Frontier { index, previous_interaction_id }`
- TTL eviction removes stale nodes and hash-index positions

**Quality Gate:** `cargo test --locked` — Phase 2 tests pass

---

## Phase 3: Versioned Persistence and SessionInfo

### 3.1 RED — V2 store round-trips sessions/interactions/in-flight
- Save store document with `version = 2`
- Load it back
- THEN sessions, interaction nodes, in-flight batches, and rebuilt hash index match

### 3.2 RED — Old v1 session file is ignored
- GIVEN old TOML with `interaction_id`, `message_count`, `pending`
- WHEN loading
- THEN load succeeds with empty v2 stores and warning path exercised

### 3.3 RED — SessionInfo does not drive frontier
- GIVEN `SessionInfo.last_interaction_id = int-old`
- AND frontier returns `int-new`
- THEN request building uses `int-new`

### 3.4 GREEN — Replace old SessionState store
- Remove `SessionState`, `message_count`, `pending`
- Add versioned store document
- Add `SessionInfo`, `InteractionStore`, `InFlightStore` persistence
- Remove `compute_delta`, `pending_sessions`, `clear_pending`

**Quality Gate:** `cargo test --locked` — Phase 3 tests pass

---

## Phase 4: InFlightStore State Machine

### 4.1 RED — Piece state transitions persist
- Pending -> Sent -> Acked
- THEN each transition is saved to store document

### 4.2 RED — Complete batch inserts terminal node only
- One harness message splits into P0/P1
- P0 ACKs `int-A`, P1 ACKs `int-B`
- THEN `InteractionStore` indexes hash only for terminal `int-B`

### 4.3 RED — Failed piece cancels ACKed pieces
- P0 ACKed, P1 fails
- THEN `POST /int-A/cancel` is called best-effort
- AND no terminal node is inserted

### 4.4 RED — Retry reuses matching in-flight batch
- Same `session_id + prev_interaction_id + message_hashes` arrives during incomplete split
- THEN no duplicate batch is created

### 4.5 GREEN — Implement InFlightStore
- `create_batch`
- `mark_sent`
- `ack_piece`
- `fail_batch`
- `complete_batch`
- `find_matching_batch`

**Quality Gate:** `cargo test --locked` — Phase 4 tests pass

---

## Phase 5: Handler Frontier Integration

### 5.1 RED — Anthropic continuation uses hash frontier
- Known Anthropic user hashes `[0xA, 0xB]` ending at `int-2`
- Incoming user hashes `[0xA, 0xB, 0xC]`
- THEN only `0xC` is sent and `previous_interaction_id = int-2`

### 5.2 RED — Anthropic rewrite with same count does not reuse stale id
- Same session has previous metadata
- Incoming first hash differs from any prefix
- THEN no previous interaction id is sent

### 5.3 RED — OpenAI assistant messages ignored for frontier
- OpenAI history includes assistant message between user/tool messages
- THEN assistant is not counted or sent in hash delta

### 5.4 RED — All-known request replays terminal interaction
- Incoming harness hashes all match known chain
- THEN handler calls replay path and makes no create-interaction call

### 5.5 RED — First-interaction fields follow frontier
- No known prefix -> tools/system/generation config present
- Known prefix -> those fields absent

### 5.6 GREEN — Replace `compute_delta` in `handle_from_anthropic` and `handle_from_openai`
- Strip controls
- Filter/hash harness messages
- Find frontier
- Build request from unknown messages only
- Update InteractionStore and SessionInfo after terminal success

**Quality Gate:** `cargo test --locked` — Phase 5 tests pass

---

## Phase 6: Split-Send Preservation and State Rewrite

### 6.1 RED — Full-body proxy_limit packing preserved
- Chunk sizes are measured as serialized full `CreateModelInteractionParams`
- Every chunk body is `<= proxy_limit`

### 6.2 RED — System instruction split still precedes content
- Oversized system instruction splits first
- First system chunk carries tools/generation config
- Later chunks chain via previous interaction id

### 6.3 RED — Split-send terminal node owns original harness hashes
- Multi-piece split completes
- THEN final interaction id owns original message hashes

### 6.4 RED — Split-send failure cancels and records failed batch
- Later piece fails
- THEN ACKed pieces are cancelled and batch is failed

### 6.5 GREEN — Rewrite `handle_split_send` around InFlightStore
- Preserve existing packing helpers where possible
- Persist before sending each piece
- ACK pieces after success
- Insert terminal node once
- Remove old per-chunk `message_count` updates

**Quality Gate:** `cargo test --locked` — Phase 6 tests pass

---

## Phase 7: Streaming Split-Send Buffer

### 7.1 RED — MemSseBuffer push/substitute/drain
- Push SSE bytes with intermediate ids
- Substitute ids with final id
- Drain returns only final id references

### 7.2 RED — Buffer overflow fails safely
- Buffer exceeds 100 MB
- THEN error is returned and ACKed pieces are cancelled

### 7.3 RED — Anthropic split streaming emits one coherent final-id stream
- P0 -> `int-A`, P1 -> `int-B`
- Client stream contains `int-B`, not `int-A`

### 7.4 RED — OpenAI split streaming emits final-id chat chunks
- Same upstream pieces
- OpenAI client receives chat-completion chunks derived after substitution

### 7.5 GREEN — Implement buffered streaming split-send
- Add `SseBuffer`/`MemSseBuffer`
- Buffer every piece SSE until final id known
- Substitute intermediate ids
- Translate/drain one client-visible stream

**Quality Gate:** `cargo test --locked` — Phase 7 tests pass

---

## Phase 8: Startup Recovery and Control Messages

### 8.1 RED — Startup rebuilds hash index from persisted interactions
- Persist nodes without runtime hash index
- Load store
- THEN hash lookup works

### 8.2 RED — Startup resumes pending in-flight piece
- Persist batch with P0 Acked and P1 Pending plus request data
- Startup resends P1 with previous id from P0

### 8.3 RED — Clean-all clears all new stores
- Sessions, interaction nodes, hash index, and in-flight batches exist
- Clean-all processed
- THEN all local stores are empty after best-effort upstream cleanup

### 8.4 RED — Extend-lifetime updates metadata and current interaction node
- Current request matches known terminal node
- Extend-lifetime processed
- THEN SessionInfo and node expiry update

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

- [ ] All 9 phases complete
- [ ] Source specs updated by `/openspec-archive`
- [ ] Ready for merge
