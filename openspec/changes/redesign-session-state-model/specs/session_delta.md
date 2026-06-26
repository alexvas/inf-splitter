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

Hash input is `serde_json::to_vec(message)` from the parsed `serde_json::Value` after control stripping. Hash algorithm is `xxh3-64`.

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

### Requirement: Branch-Safe InteractionStore

`InteractionStore` MUST model upstream interactions as a tree and MUST support duplicate hashes and forks.

```rust
struct InteractionNode {
    id: String,
    prev_id: Option<String>,
    message_hashes: Vec<u64>,
    last_seen_utc: u64,
    expires_at_utc: u64,
}

struct InteractionPosition {
    interaction_id: String,
    message_index: usize,
}

struct InteractionStore {
    nodes: HashMap<String, InteractionNode>,
    hash_index: HashMap<u64, Vec<InteractionPosition>>,
}
```

Operations:
- `insert(node)` — adds/updates node and indexes every `message_hashes` position.
- `get(id) -> Option<&InteractionNode>`.
- `lookup_hash(hash) -> &[InteractionPosition]`.
- `walk_chain(id) -> Vec<&InteractionNode>` from leaf to root.
- `evict_expired(now)` removes expired nodes and stale hash-index positions.

`hash_index` MUST NOT be single-valued. Duplicate content and branch collisions are valid inputs.

#### Scenario: Duplicate hash in two branches
- GIVEN branch A has node `int-A` with hash `0xH`
- AND branch B has node `int-B` with hash `0xH`
- WHEN `lookup_hash(0xH)` is called
- THEN both positions are returned

#### Scenario: Chain walking
- GIVEN `int-1 -> int-2 -> int-3`
- WHEN walking from `int-3`
- THEN nodes are returned leaf-to-root: `[int-3, int-2, int-1]`

### Requirement: Longest Valid Prefix Frontier

For stateless clients, frontier selection MUST choose the longest contiguous prefix of incoming harness-message hashes that appears in one valid interaction chain in order.

Algorithm requirements:
1. Search candidate chains using multi-valued `hash_index` positions.
2. Validate ordered prefix membership against concrete `InteractionNode.message_hashes` in chain order.
3. Ignore isolated hash matches that are not part of the same ordered prefix.
4. Return `(frontier_index, previous_interaction_id)` where `frontier_index` is the first unknown message.
5. If all messages are known, `previous_interaction_id` is replay target.
6. Tie-break same-length candidates by newest `last_seen_utc`, then lexicographically smallest `interaction_id`.

#### Scenario: Known prefix trimmed
- GIVEN chain `int-1 {hashes: [0xA]}` -> `int-2 {hashes: [0xB]}`
- WHEN incoming hashes are `[0xA, 0xB, 0xC]`
- THEN frontier is `2` and previous interaction is `int-2`

#### Scenario: Isolated later hash does not move frontier
- GIVEN store has hash `0xB` only in an unrelated branch
- WHEN incoming hashes are `[0xA, 0xB]`
- THEN frontier is `0` because prefix `0xA` is unknown

#### Scenario: All known replays terminal interaction
- GIVEN chain contains incoming hashes `[0xA, 0xB]`
- WHEN frontier selection runs
- THEN frontier is `2` and replay target is terminal interaction for the matched chain

#### Scenario: Equal prefix tie-break is deterministic
- GIVEN two chains both match prefix `[0xA, 0xB]`
- AND both have equal `last_seen_utc`
- WHEN frontier selection runs
- THEN lexicographically smallest terminal interaction id wins

### Requirement: Durable InFlightStore

`InFlightStore` MUST persist split-send progress before and after every piece status transition.

```rust
struct InFlightBatch {
    id: String,
    session_id: String,
    prev_interaction_id: Option<String>,
    message_hashes: Vec<u64>,
    pieces: Vec<InFlightPiece>,
    terminal_result: Option<String>,
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

`message_hashes` are original harness-message hashes. `content_hash` identifies a split piece only. `request_body` is persisted before send so Pending pieces can be resent during startup recovery. Intermediate piece interactions MUST NOT own original harness-message hashes in `InteractionStore`.

A batch is complete when every piece is `Acked`. On completion, insert one terminal `InteractionNode`:
- `id = final Acked interaction_id`
- `prev_id = batch.prev_interaction_id`
- `message_hashes = batch.message_hashes`

#### Scenario: Two pieces complete as one terminal node
- GIVEN batch has `message_hashes = [0xH0]` and two pieces
- WHEN P0 ACKs `int-A` and P1 ACKs `int-B`
- THEN only terminal node `int-B` is inserted with `message_hashes = [0xH0]`
- AND no node for `int-A` owns `0xH0`

#### Scenario: Retry matches existing in-flight batch
- GIVEN an incomplete batch with `session_id = sess-1`, `prev_interaction_id = int-0`, and `message_hashes = [0xA]`
- WHEN the same request arrives again
- THEN the handler reuses that batch instead of creating a second split sequence

#### Scenario: Failed piece cancels ACKed pieces
- GIVEN P0 is `Acked { interaction_id: "int-A" }`
- WHEN P1 fails
- THEN the handler calls `POST /int-A/cancel`, marks batch failed, and does not insert an `InteractionNode`

### Requirement: Versioned Persistence Document

The file configured by `interactions_session_store` MUST store a versioned document:

```toml
version = 2

[sessions]
# client_session_id = SessionInfo

[interactions]
# interaction_id = InteractionNode

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

`SessionInfo` MUST NOT drive frontier selection or `previous_interaction_id`. Handlers update it after successful terminal interaction replay or creation so logs and response-header behavior remain inspectable.

#### Scenario: SessionInfo does not choose route frontier
- GIVEN SessionInfo points to `int-old`
- AND InteractionStore longest valid prefix points to `int-new`
- WHEN building the upstream request
- THEN `previous_interaction_id = int-new`

## MODIFIED

### Requirement: SessionState Replacement

`SessionState { interaction_id, message_count, pending }` is removed. Its responsibilities are split:
- `InteractionStore` chooses chain frontier.
- `InFlightStore` tracks partial split-send progress.
- `SessionInfo` records client-session metadata only.

## REMOVED

- `compute_delta(delivered, incoming)`.
- `pending: bool`.
- `SessionStore::pending_sessions()`.
- `SessionStore::clear_pending()`.
