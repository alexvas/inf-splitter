# Proposal: Redesign Session State Model

**Change ID:** `redesign-session-state-model`
**Created:** 2026-06-24
**Updated:** 2026-06-25
**Status:** Draft
**Depends on:** `fix-interactions-protocol-correctness`, `fix-header-correlation-mapping`

---

## Problem Statement

Current interactions session tracking uses `{interaction_id, message_count, pending}`. That model is structurally wrong for current clients and split-send behavior:

1. `message_count` counts raw client messages, but stateless protocols send both harness-originated messages (`user`, `system`, `developer`, `tool`) and LLM-originated history (`assistant`). Only harness-originated messages should drive upstream deltas.
2. `message_count` cannot distinguish history rewrite/fork from ordinary continuation. Same client session and same count can contain different content, which causes stale `previous_interaction_id` reuse.
3. `pending: bool` cannot represent split-send partial progress. One harness message can become multiple upstream interactions; some may be ACKed while others fail or remain unsent.
4. Streaming split-send currently cannot expose one coherent client stream when intermediate chunks create temporary `interaction.id` values.
5. Startup recovery only verifies a single pending final interaction; it cannot replay or verify per-piece in-flight split state.

## Proposed Solution

Replace count-based session state with three explicit stores:

- `InteractionStore` — durable tree of known upstream interactions and the harness-message hashes delivered by each terminal interaction.
- `InFlightStore` — durable per-batch/per-piece state for split sends that have not reached terminal success or failure.
- `SessionStore<SessionInfo>` — durable client-session metadata for logging, response headers, and incident investigation. It does not choose routing frontier.

### Canonical harness message hashing

Before hashing, the handler strips in-band control messages. It then filters harness-originated messages by ingress protocol:

| Protocol | Kept | Discarded |
|----------|------|-----------|
| Anthropic Messages | `user` messages, including `tool_result` blocks | `assistant` |
| OpenAI Chat Completions | `system`, `developer`, `user`, `tool` messages | `assistant` |

Each kept message is serialized in full with `serde_json::to_vec` from the parsed `serde_json::Value` after control stripping — the entire message `Value` (all fields including `role`, `content`, nested `tool_result` blocks) is serialized, not extracted text. The `xxh3-64` of those bytes is the message hash. Hash collisions are handled by storing all candidate positions for a hash and validating chain order, not by trusting a single hash lookup.

### Two-model InteractionStore

`InteractionStore` separates the client-visible logical chain from the upstream physical chain:

```rust
struct ClientInteractionNode {
    id: String,                  // terminal upstream id, client-visible
    prev_id: Option<String>,     // previous ClientInteractionNode.id
    message_hashes: Vec<u64>,    // pre-split harness message hashes
    system_instruction_hash: Option<u64>, // hash of first-interaction system_instruction; None for follow-ups
    upstream_ids: Vec<String>,   // all backing UpstreamInteractionNode.id's
    last_seen_utc: u64,
}

struct UpstreamInteractionNode {
    id: String,                  // upstream-assigned interaction id
    prev_id: Option<String>,     // previous UpstreamInteractionNode.id
    client_id: String,           // diagnostic: {request_id} or {request_id}:{chunk-N}
    last_seen_utc: u64,
    expires_at_utc: u64,
}

struct ClientInteractionPosition {
    client_id: String,
    message_index: usize,
}

struct InteractionStore {
    clients: HashMap<String, ClientInteractionNode>,
    upstreams: HashMap<String, UpstreamInteractionNode>,
    hash_index: HashMap<u64, Vec<ClientInteractionPosition>>,
}
```

Key properties:
- `UpstreamInteractionNode.client_id` is diagnostic-only. The mapping client→upstream lives exclusively in `ClientInteractionNode.upstream_ids`.
- `UpstreamInteractionNode.last_seen_utc` is updated on creation and on every GET replay that traverses this node.
- `ClientInteractionNode` is created AFTER all upstream pieces complete. It's a denormalized view — `upstream_ids` lists all backing upstream nodes in chain order.
- `ClientInteractionNode.system_instruction_hash` stores the xxh3 hash of system_instruction from the first interaction in chain (when `prev_id` is `None`). Follow-up interactions set it to `None`. If a follow-up request's system_instruction hash differs from the root node's stored hash, the handler forks.
- `hash_index` indexes only `ClientInteractionNode.message_hashes` — never piece hashes or upstream nodes. Multi-valued: duplicate content and branch collisions are valid.
- Frontier works exclusively with client nodes. Upstream nodes are never consulted for routing.

### Stateless frontier selection

Stateless clients (Anthropic Messages and OpenAI Chat Completions) resend full visible history and do not know Gemini `interaction.id`. The proxy:

1. strips control messages;
2. filters harness-originated messages;
3. hashes each canonical message;
4. searches `InteractionStore` client chain for the longest contiguous prefix `[0..k)` that belongs to one valid client interaction chain in order;
5. if prefix ends at a client interaction **boundary**: `previous_interaction_id = that client node's id`, forward only messages from `k` onward;
6. if prefix ends **inside** a client interaction: fork at the client node's `prev_id`, forward all messages from that node's first message onward;
7. when `k == len` (all messages known), require incoming `prev_id` to equal the matched `ClientInteractionNode.prev_id`; then read `ClientInteractionNode.upstream_ids`, fetch all upstream interactions via GET, merge `steps[]` in piece order (last piece's `usage`, first piece's `tools`/`system_instruction`/`generation_config`), return one response with the client node's `id`.

If duplicate/collision candidates leave multiple fully validated chains with same longest prefix, choose the one with newest `last_seen_utc`; if still tied, choose lexicographically smallest `interaction_id` for deterministic behavior. Normal duplicate-content disambiguation uses chain validation: lookup by hash, then identify the client node by `(prev_id, message_hash)` in chain order. Tuple `[Some(prev_id), message_hash]` identifies a client node equivalently to `id`.

### Stateful future path

OpenAI Responses → Gemini is not implemented by this change. Store APIs are shaped so a future stateful path can validate `prev_interaction_id` directly and skip hash trimming. No request handler for OpenAI Responses is added in scope.

### Split-send and InFlightStore

```rust
struct InFlightBatch {
    id: String,
    session_id: String,
    prev_interaction_id: Option<String>,
    message_hashes: Vec<u64>,
    pieces: Vec<InFlightPiece>,
    created_utc: u64,
    updated_utc: u64,
}

struct InFlightPiece {
    index: usize,
    content_hash: u64,
    request_body: Vec<u8>,
    status: InFlightStatus,
}

enum InFlightStatus {
    Pending,
    Sent { request_hash: u64, interaction_id: Option<String> },
    Acked { interaction_id: String },
    Failed { error: String },
}
```

`message_hashes` are original harness-message hashes. `content_hash` is piece identity only. `request_body` is persisted before send so Pending pieces can be resent during startup recovery. Intermediate split pieces create upstream interactions but do not own harness-message hashes in `InteractionStore`.

When all pieces are `Acked`, the batch completes:
1. Insert all `UpstreamInteractionNode`s for each `Acked { interaction_id }` in piece order, linked by `prev_id`.
2. Insert one `ClientInteractionNode` with `id = final Acked interaction_id`, `prev_id = batch.prev_interaction_id`, `message_hashes = batch.message_hashes`, and `upstream_ids = [all Acked interaction_ids in piece order]`.
3. Remove the batch from `InFlightStore`.

When a client retries with same `message_hashes` and frontier hits a client node with multiple `upstream_ids`, the handler fetches all upstream interactions via GET, merges their content into one composite response, and returns it with the client node's `id`. Only the terminal upstream id is ever visible to the client.

If a client retries while a batch is incomplete, matching is by `session_id + message_hashes + prev_interaction_id`. The handler waits for completion when possible.

### Deterministic split packing

Existing full-body `proxy_limit` semantics are preserved:

1. Measure full serialized `CreateModelInteractionParams`, not content-only bytes.
2. Split `system_instruction` first when first-envelope + system instruction exceeds limit.
3. First system-instruction chunk carries `tools`, `generation_config`, and first part of `system_instruction`.
4. Each subsequent system-instruction chunk carries its part of `system_instruction` + `previous_interaction_id`; `tools` and `generation_config` are absent.
5. The last system-instruction chunk can also pack content items when they fit within `proxy_limit`.
6. Chunk sizing accounts for serialized `previous_interaction_id` overhead.
7. Every emitted chunk body must be `<= proxy_limit` or the request fails before sending.

### Streaming split-send

For `stream: true` split-send, all upstream piece SSE streams are buffered until the final interaction id is known. The proxy substitutes every upstream `interaction.id` / client-visible message id from intermediate pieces with the final id, then drains one coherent translated SSE stream to the client.

Initial buffer implementation is memory-backed with a 100 MB cap. On overflow, the handler cancels ACKed pieces, marks the batch failed, and returns an error. Disk-backed buffering remains out of scope.

### Negative scenarios

| Scenario | Handling |
|----------|----------|
| Piece N fails or times out | Cancel ACKed piece interactions via `POST /{id}/cancel`, mark batch failed, do not insert ClientInteractionNode, return error. |
| Client disconnects mid-split | Continue batch to completion. Insert `UpstreamInteractionNode`s + `ClientInteractionNode`. Retry fetches via `upstream_ids`, merges, returns. |
| Client resends same in-flight split | Find matching batch. If running, wait. If completed, fetch via `ClientInteractionNode.upstream_ids`, merge, return. |
| Crash mid-split | Load persisted InFlightStore; ACKed pieces are trusted. Sent pieces with interaction_id trigger GET to re-fetch the interaction from upstream — if it exists, drain and transition to Acked; if 404 or error, mark Failed. Pending pieces are resent. Failed batches remain until clean-all or future expiration cleanup. |
| SSE buffer overflow | Cancel ACKed piece interactions, mark batch failed, return error. |
| Hash collision or duplicate content | `hash_index` returns candidates; chain order validation chooses valid longest prefix, never single hash alone. |

## Persistence and migration

The existing `interactions_session_store` config path remains the persistence location. File format changes from old `HashMap<String, SessionState>` to a versioned top-level document containing:

- `version = 2`
- `sessions`
- `interactions`
- `in_flight`

Old version-1 files are not migrated because they lack content hashes. On startup, the proxy logs a warning, ignores old count-based sessions, and overwrites with the v2 format on next save.

Clean-all control cancels/deletes known terminal interactions, cancels ACKed in-flight pieces, and clears all three stores. Expiration cleanup for sessions, interaction nodes, hash-index positions, and in-flight batches is out of scope for this change and should be specified by a dependent change.

## Scope

### In Scope

- Harness message filtering after control-message stripping.
- Canonical `xxh3-64` hashing of filtered messages.
- Branch/collision-safe `InteractionStore` and frontier selection.
- Durable `InFlightStore` with per-piece status and terminal result fetching.
- Versioned persistence and old-session invalidation.
- `SessionInfo` replacing old `SessionState` for metadata only.
- Existing split-send full-body packing invariants preserved.
- Streaming split-send buffering and final-id substitution.
- Startup recovery for persisted in-flight batches.
- Existing response session headers preserved.
- Clean-all / extend-lifetime control behavior updated for new stores.

### Out of Scope

- OpenAI Responses ingress implementation.
- Disk-backed SSE buffer.
- Summarization/compaction.
- Migrating old count-based session files to hashed chains.

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/session.rs` | Complete rewrite | Versioned store document, `InteractionStore`, `InFlightStore`, `SessionInfo`, old format invalidation. |
| `src/interactions_handler.rs` | Major | Control stripping before hashing, frontier selection, split-send state machine, replay, streaming buffering, cancellation. |
| `src/interactions.rs` | Major | Harness filtering, canonical hashing helpers, preserve full-body split packing. |
| `src/sse.rs` | Minor | Memory SSE buffer and id substitution utilities. |
| `src/lib.rs` | Major | Startup recovery loads v2 stores and resumes/verifies in-flight batches. |
| `src/config.rs` | Minor | Document changed meaning of `interactions_session_store`; no new config key. |
| `Cargo.toml`/lockfile | Minor | Add `xxhash-rust`. |
| Tests | Major | Session model, frontier selection, split/recovery/control/header regression tests. |

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Hash collision / duplicate message content | Low | High | Multi-candidate index + ordered chain validation; tests for duplicate content. |
| Frontier selection chooses wrong branch | Medium | High | Longest valid prefix, deterministic tie-break, tests for forks and rewrites. |
| Old session files ignored | Medium | Medium | Log explicit warning; safe reset beats stale `previous_interaction_id`. |
| Streaming buffer too large | Low | Medium | 100 MB cap, cancel ACKed pieces, fail clearly. |
| Recovery resends duplicate piece | Low | High | Persist request hash/status before send; verify Sent pieces via GET before resend. |
| Cancel API behavior differs | Low | High | Use existing `POST /{id}/cancel`; tolerate 404 during cleanup. |
| Store grows unbounded | Medium | Medium | Follow-up expiration cleanup change will define eviction across sessions, interactions, in-flight batches, and hash_index. |
