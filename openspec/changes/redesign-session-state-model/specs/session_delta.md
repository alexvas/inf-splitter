# Delta: Session State Model Internals

**Change ID:** `redesign-session-state-model`
**Affects:** `src/session.rs`, `src/interactions_handler.rs`, `src/interactions.rs`, `src/sse.rs`, `src/lib.rs`, `Cargo.toml`

---

## ADDED

### Requirement: Canonical Harness Message Hashing

The proxy MUST hash only harness-originated messages after in-band control messages are stripped.

```rust
fn filter_harness_messages(messages: &[Value], protocol: Protocol) -> Vec<Value>
fn hash_harness_message(message: &Value) -> u64
```

| Protocol | Kept | Discarded |
|----------|------|-----------|
| Anthropic Messages | `user` role, including `tool_result` blocks | `assistant` |
| OpenAI Chat Completions | `system`, `developer`, `user`, `tool` | `assistant` |

Hash input is `serde_json::to_vec(message)` from the parsed `serde_json::Value` after control stripping — the **full** message `Value` (all fields including `role`, `content`, nested `tool_result` blocks) is serialized and hashed, not extracted text. Hash algorithm is `xxh3-64`.

#### Scenario: Anthropic user kept, assistant discarded
- GIVEN messages `[{role: "user"}, {role: "assistant"}, {role: "user"}]`
- WHEN filtering for Anthropic
- THEN only the two `user` messages are returned

#### Scenario: OpenAI harness roles kept
- GIVEN messages `[{role: "system"}, {role: "user"}, {role: "assistant"}, {role: "tool"}, {role: "developer"}]`
- WHEN filtering for OpenAI
- THEN `system`, `user`, `tool`, and `developer` are returned

#### Scenario: Control stripped before hashing
- GIVEN an incoming message list containing a clean-all control message and one user message
- WHEN control scanning runs before harness hashing
- THEN only the user message contributes to message hashes

### Requirement: Two-Model InteractionStore

`InteractionStore` separates the client-visible logical chain from the upstream physical chain. Two node types, one store:

```rust
/// Client-visible logical interaction. Created AFTER all upstream
/// pieces complete.
struct ClientInteractionNode {
    /// Terminal upstream id — client-visible. Always equals
    /// upstream_ids.last().
    id: String,
    /// Previous ClientInteractionNode.id in the logical chain.
    prev_id: Option<String>,
    /// Pre-split xxh3 hashes of harness messages delivered in this
    /// logical interaction.
    message_hashes: Vec<u64>,
    /// xxh3 hash of the system_instruction sent in the first interaction
    /// of this chain (None for follow-up interactions where prev_id is Some).
    /// Used to detect mid-chain system_instruction changes and trigger a fork.
    system_instruction_hash: Option<u64>,
    /// All backing UpstreamInteractionNode.id's, in chain order.
    /// Single element when no split occurred.
    upstream_ids: Vec<String>,
    last_seen_utc: u64,
}

/// Physical upstream interaction. One per actual upstream API call.
/// Exists independently of ClientInteractionNode — upstream nodes
/// are created during split-send, client node only after completion.
struct UpstreamInteractionNode {
    /// interaction.id assigned by upstream.
    id: String,
    /// Previous UpstreamInteractionNode.id in the physical chain.
    prev_id: Option<String>,
    /// Diagnostic back-reference for incident investigation.
    /// Format: `{client_request_id}` for single-piece; `{client_request_id}:{chunk-N}` for split.
    client_id: String,
    /// Updated on creation and on every GET replay that references this node.
    last_seen_utc: u64,
    expires_at_utc: u64,
}

/// Position of a harness-message hash within a ClientInteractionNode.
/// hash_index ONLY indexes ClientInteractionNode.message_hashes —
/// upstream piece hashes are never indexed.
struct ClientInteractionPosition {
    client_id: String,
    message_index: usize,
}

struct InteractionStore {
    clients: HashMap<String, ClientInteractionNode>,
    upstreams: HashMap<String, UpstreamInteractionNode>,
    /// message_hash → client interaction positions.
    /// Multi-valued: duplicate content and branch collisions are valid.
    hash_index: HashMap<u64, Vec<ClientInteractionPosition>>,
}
```

Key invariants:
- `ClientInteractionNode` is created ONLY after ALL `upstream_ids` nodes are persisted in `upstreams`.
- `hash_index` references ONLY `ClientInteractionNode.message_hashes` — never piece hashes, never upstream nodes.
- `UpstreamInteractionNode.client_id` is diagnostic-only. The mapping client→upstream lives exclusively in `ClientInteractionNode.upstream_ids`.
- `UpstreamInteractionNode.last_seen_utc` is updated on creation AND on every GET replay that traverses this node.
- `ClientInteractionNode.system_instruction_hash` is set to `Some(hash)` on the first interaction of a chain (when `prev_id` is `None`), and `None` on follow-ups. If a follow-up request has `prev_id=Some(...)` but the client's current `system_instruction` xxh3 hash differs from the root interaction's stored `system_instruction_hash`, the handler MUST log an error and fork (`prev_id=None`).

Operations:
- `insert_upstream(node)` — adds upstream node.
- `insert_client(node)` — adds client node and indexes every `message_hashes` position into `hash_index`.
- `get_client(id) -> Option<&ClientInteractionNode>`.
- `get_upstream(id) -> Option<&UpstreamInteractionNode>`.
- `lookup_hash(hash) -> &[ClientInteractionPosition]`.
- `walk_client_chain(id) -> Vec<&ClientInteractionNode>` from leaf to root.
- `walk_upstream_chain(id) -> Vec<&UpstreamInteractionNode>` from leaf to root.

Expiration cleanup is out of scope for this change and will be specified by a dependent change.

#### Scenario: Single upstream — single client
- GIVEN no split, one upstream call creates `int-A`
- WHEN interaction completes
- THEN `UpstreamInteractionNode { id: "int-A", prev_id: ... }` is inserted
- AND `ClientInteractionNode { id: "int-A", upstream_ids: ["int-A"], message_hashes: [...] }` is inserted
- AND `hash_index` maps each message_hash → `("int-A", index)`

#### Scenario: Split into two — one client, two upstreams
- GIVEN harness message H1 split into two pieces
- WHEN P0 ACKs `int-B`, P1 ACKs `int-D` (with system_instruction chunk `int-C` between)
- THEN `UpstreamInteractionNode`s: `int-B → int-C → int-D`
- AND `ClientInteractionNode { id: "int-D", upstream_ids: ["int-B", "int-C", "int-D"], message_hashes: [hash(H1)] }`
- AND `hash_index` maps `hash(H1)` → `("int-D", 0)`

#### Scenario: Chain walking — client chain
- GIVEN `C1 → C2 → C3`
- WHEN walking client chain from `C3`
- THEN nodes returned leaf-to-root: `[C3, C2, C1]`

### Requirement: Longest Valid Prefix Frontier

For stateless clients, frontier selection MUST choose the longest contiguous prefix of incoming harness-message hashes that appears in one valid **client** interaction chain in order. Frontier works exclusively with `ClientInteractionNode` and `hash_index` — upstream nodes are never consulted for routing.

Algorithm requirements:
1. Hash each filtered harness message.
2. Look up each hash in `hash_index` to get candidate `(client_id, message_index)` positions.
3. Validate ordered prefix membership against concrete `ClientInteractionNode.message_hashes` in client chain order.
4. Ignore isolated hash matches not part of the same ordered prefix.
5. If prefix ends at a client interaction **boundary** (last matched message is last in its client node): `previous_interaction_id` is that client node's `id`.
6. If prefix ends **inside** a client interaction: fork at the client node's `prev_id`.
7. Return `(frontier_index, previous_interaction_id)`.
8. If all messages are known: require incoming `prev_id` to equal the matched `ClientInteractionNode.prev_id`, then read `ClientInteractionNode.upstream_ids`, fetch all upstream interactions via GET, merge `steps[]` in piece order (last piece's `usage`, first piece's `tools`/`system_instruction`/`generation_config`), return one response with the client node's `id`.
9. Normal duplicate-content disambiguation validates chain order using `(prev_id, message_hash)`; tuple `[Some(prev_id), message_hash]` identifies the client node equivalently to `id`.
10. If multiple fully validated candidates still have same prefix length, tie-break by newest `last_seen_utc`, then lexicographically smallest `id`.

#### Scenario: Known prefix trimmed at client boundary
- GIVEN client chain `C1 {hashes: [0xA]}` -> `C2 {hashes: [0xB]}`
- WHEN incoming hashes are `[0xA, 0xB, 0xC]`
- THEN frontier is `2` and previous interaction is `C2.id`
- AND only `0xC` is forwarded

#### Scenario: Frontier inside client node — fork at parent
- GIVEN `C1 {hashes: [0xA, 0xB, 0xC]}` with `prev_id = C0.id`
- WHEN incoming hashes are `[0xA, 0xB, 0xD]` with incoming `prev_id = C0.id`
- THEN `0xA, 0xB` match positions 0,1 inside C1
- AND `0xC` is absent from incoming (suffix divergence)
- THEN `previous_interaction_id = C0.id` (fork at C1's parent)
- AND messages for `[0xA, 0xB, 0xD]` are forwarded

#### Scenario: Frontier inside multi-node client chain
- GIVEN `C1 {hashes: [0xA]}` -> `C2 {hashes: [0xB, 0xC]}`
- WHEN incoming hashes are `[0xA, 0xB, 0xD]`
- THEN `0xA` matches C1 boundary, `0xB` matches C2 position 0
- AND `0xC` is absent from incoming (C2 suffix divergence)
- THEN `previous_interaction_id = C1.id` (fork at C2's parent)
- AND messages for `[0xB, 0xD]` are forwarded

#### Scenario: Isolated later hash does not move frontier
- GIVEN store has hash `0xB` only in an unrelated client branch
- WHEN incoming hashes are `[0xA, 0xB]`
- THEN frontier is `0` because prefix `0xA` is unknown

#### Scenario: All known — fetch and merge from all upstream nodes
- GIVEN `ClientInteractionNode { id: "int-B", prev_id: Some("int-0"), upstream_ids: ["int-A", "int-B"], message_hashes: [0xA, 0xB] }`
- AND incoming hashes are `[0xA, 0xB]`
- AND incoming `prev_id == Some("int-0")`
- THEN frontier is `2`
- AND handler reads `upstream_ids`, fetches `GET /int-A` and `GET /int-B` from upstream
- AND merges content from both into one response with id `int-B`

#### Scenario: All known — no split, single upstream
- GIVEN `ClientInteractionNode { id: "int-A", prev_id: Some("int-0"), upstream_ids: ["int-A"], message_hashes: [0xH0] }`
- AND incoming hashes are `[0xH0]`
- AND incoming `prev_id == Some("int-0")`
- THEN frontier is `1`
- AND handler fetches `GET /int-A`, translates response — no merge needed

#### Scenario: Equal validated-chain tie-break is deterministic
- GIVEN duplicate/collision candidates leave two fully validated client chains with same prefix length
- AND both chains have equal `last_seen_utc`
- WHEN frontier selection runs
- THEN lexicographically smallest terminal interaction id wins
- NOTE: This is only a deterministic fallback after chain validation. Normal duplicate-content disambiguation uses `(prev_id, message_hash)` in chain order; tuple `[Some(prev_id), message_hash]` identifies the client node equivalently to `id`.

### Requirement: Durable InFlightStore

`InFlightStore` MUST persist split-send progress via `save_to_disk()` after every piece status transition (Pending→Sent, Sent→Acked, any→Failed) and on batch creation/completion.

#### Scenario: Persistence after every piece status change
- GIVEN an InFlightBatch with piece P0 Pending
- WHEN P0 transitions to Sent
- THEN `save_to_disk()` is called immediately
- WHEN P0 transitions to Acked
- THEN `save_to_disk()` is called immediately

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
    /// HTTP 200 received, SSE stream in progress or completed but not fully drained.
    /// On recovery, proxy MUST re-fetch interaction via GET to verify completion
    /// and re-drain content rather than re-sending. If GET fails or interaction
    /// is gone, treat as Failed.
    Sent { request_hash: u64, interaction_id: Option<String> },
    /// SSE stream fully consumed, all response content collected, no more events expected.
    Acked { interaction_id: String },
    Failed { error: String },
}
```

`message_hashes` are original harness-message hashes. `content_hash` identifies a split piece only. `request_body` is persisted before send so Pending pieces can be resent during startup recovery.

A batch is complete when every piece is `Acked`. On completion:
1. Insert all `UpstreamInteractionNode`s for each `Acked { interaction_id }` in piece order, linked by `prev_id` (first piece's `prev_id = batch.prev_interaction_id`).
2. Insert one `ClientInteractionNode`:
   - `id = final Acked interaction_id`
   - `prev_id = batch.prev_interaction_id`
   - `message_hashes = batch.message_hashes`
   - `upstream_ids = [all Acked interaction_ids in piece order]`
3. Remove the batch from `InFlightStore`.

When a subsequent client request hits the same `message_hashes` and frontier selects the client node, the handler reads `upstream_ids`, fetches all upstream interactions via GET, merges their content, and returns one response with the client node's `id`.

#### Scenario: Two pieces complete — upstream nodes + one client node
- GIVEN batch has `message_hashes = [0xH0]`, `prev_interaction_id = None`, two pieces
- WHEN P0 ACKs `int-A` and P1 ACKs `int-B`
- THEN `UpstreamInteractionNode`s inserted: `{id: "int-A", prev_id: None}`, `{id: "int-B", prev_id: "int-A"}`
- AND `ClientInteractionNode` inserted: `{id: "int-B", prev_id: None, message_hashes: [0xH0], upstream_ids: ["int-A", "int-B"]}`
- AND batch is removed from `InFlightStore`

#### Scenario: Single piece — one upstream node, one client node
- GIVEN batch has one piece ACKing `int-A`
- THEN `UpstreamInteractionNode {id: "int-A", ...}` and `ClientInteractionNode {id: "int-A", upstream_ids: ["int-A"], ...}` are inserted
- AND retry fetches only `GET /int-A`

#### Scenario: Retry matches existing in-flight batch
- GIVEN an incomplete batch with `session_id = sess-1`, `prev_interaction_id = int-0`, and `message_hashes = [0xA]`
- WHEN the same request arrives again
- THEN the handler reuses that batch instead of creating a second split sequence

#### Scenario: Failed piece cancels ACKed pieces
- GIVEN P0 is `Acked { interaction_id: "int-A" }`
- WHEN P1 fails
- THEN the handler calls `POST /int-A/cancel`, marks batch failed, and does not insert an `InteractionNode`

#### Scenario: Recovery — Sent piece with interaction_id re-fetched via GET
- GIVEN persisted batch has P0 Acked `int-A`, P1 Sent `{ request_hash, interaction_id: Some("int-B") }`
- WHEN proxy starts and recovers
- THEN P0 is trusted
- AND proxy calls `GET /v1beta/interactions/int-B`
- AND if 200: drains content from the interaction, marks P1 Acked `int-B`, batch completes normally
- AND if 404: marks P1 Failed, cancels P0's `int-A`, batch failed
- AND if GET fails (network error): keeps P1 as Sent for next recovery attempt

#### Scenario: Recovery — Sent piece without interaction_id
- GIVEN persisted batch has P0 Acked `int-A`, P1 Sent `{ request_hash, interaction_id: None }`
- WHEN proxy starts and recovers
- THEN P1 is indeterminate — no interaction_id to probe
- AND P1 is marked Failed (duplicate-send prevention)
- AND P0's `int-A` is cancelled best-effort

### Requirement: Versioned Persistence Document

The file configured by `interactions_session_store` MUST store a versioned document:

```toml
version = 2

[sessions]
# client_session_id = SessionInfo

[interactions.clients]
# id = ClientInteractionNode

[interactions.upstreams]
# id = UpstreamInteractionNode

[in_flight]
# batch_id = InFlightBatch
```

Writes MUST be atomic. Old version-1 files containing only `HashMap<String, SessionState>` MUST be ignored with a warning and replaced on next save.

#### Scenario: Old session file ignored
- GIVEN persistence file contains old `SessionState` entries without `version = 2`
- WHEN the proxy starts
- THEN old sessions are ignored
- AND a warning is logged explaining that count-based sessions cannot be migrated

### Requirement: SessionInfo Metadata Store

Old `SessionState` MUST be replaced by `SessionInfo` for metadata only.

```rust
struct SessionInfo {
    client_session_id: String,
    last_interaction_id: Option<String>,
    last_seen_utc: u64,
    expires_at_utc: u64,
}
```

`SessionInfo` MUST NOT drive frontier selection or `previous_interaction_id`. Handlers update it after successful upstream interaction creation or fetch, so logs and response-header behavior remain inspectable.

#### Scenario: SessionInfo does not choose route frontier
- GIVEN SessionInfo points to `int-old`
- AND InteractionStore longest valid prefix points to `int-new`
- WHEN building the upstream request
- THEN `previous_interaction_id = int-new`

## MODIFIED

### Requirement: SessionState Replacement

`SessionState { interaction_id, message_count, pending }` is removed. Its responsibilities are split:
- `ClientInteractionNode` + `hash_index` drive chain frontier (logical client chain).
- `UpstreamInteractionNode` tracks physical upstream interactions.
- `InFlightStore` tracks partial split-send progress.
- `SessionInfo` records client-session metadata only.

## REMOVED

- `compute_delta(delivered, incoming)`.
- `pending: bool`.
- `SessionStore::pending_sessions()`.
- `SessionStore::clear_pending()`.
