//! Session state tracking for the Gemini Interactions API.
//!
//! Hash-chain frontier model: incoming harness-message hashes are matched
//! against known ClientInteractionNodes to determine which messages are
//! already delivered upstream. State is persisted to a versioned TOML file
//! (StoreV2).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Default session TTL: 12 hours in seconds.
pub const DEFAULT_SESSION_TTL_SECS: u64 = 12 * 60 * 60;

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── New v2 session state model ───────────────────────────────────────

/// Metadata about a client session — for logging, response headers,
/// and diagnostics. Does NOT drive routing frontier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub client_session_id: String,
    pub last_interaction_id: Option<String>,
    pub last_seen_utc: u64,
    pub expires_at_utc: u64,
}

/// Client-visible logical interaction. Created AFTER all upstream
/// pieces complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInteractionNode {
    /// Terminal upstream id — client-visible.
    pub id: String,
    /// Previous ClientInteractionNode.id in the logical chain.
    pub prev_id: Option<String>,
    /// Pre-split xxh3 hashes of harness messages delivered in this
    /// logical interaction.
    pub message_hashes: Vec<u64>,
    /// xxh3 hash of the system_instruction sent in the FIRST interaction
    /// of this chain. None for follow-ups (prev_id is Some).
    pub system_instruction_hash: Option<u64>,
    /// All backing UpstreamInteractionNode.id's, in chain order.
    pub upstream_ids: Vec<String>,
    pub last_seen_utc: u64,
}

/// Physical upstream interaction. One per actual upstream API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamInteractionNode {
    pub id: String,
    pub prev_id: Option<String>,
    /// Diagnostic: {client_request_id} or {client_request_id}:{chunk-N}
    pub client_id: String,
    pub last_seen_utc: u64,
    pub expires_at_utc: u64,
}

/// Status of a single split-send piece.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InFlightStatus {
    Pending,
    /// HTTP 200 received from upstream, but no interaction id observed yet.
    ResponseStarted,
    /// Upstream interaction id observed; SSE stream may still be draining.
    Sent {
        interaction_id: String,
    },
    /// SSE stream fully consumed, all content collected.
    Acked {
        interaction_id: String,
    },
    Failed {
        error: String,
    },
}

/// A single piece in a split-send batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InFlightPiece {
    pub index: usize,
    pub content_hash: u64,
    pub request_body: Vec<u8>,
    pub status: InFlightStatus,
}

/// Durable per-batch state for a split send in progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InFlightBatch {
    pub id: String,
    pub session_id: String,
    pub prev_interaction_id: Option<String>,
    pub message_hashes: Vec<u64>,
    pub system_instruction_hash: Option<u64>,
    pub pieces: Vec<InFlightPiece>,
    pub created_utc: u64,
    pub updated_utc: u64,
}

/// Position of a harness-message hash within a ClientInteractionNode.
#[derive(Debug, Clone)]
pub struct ClientInteractionPosition {
    pub client_id: String,
    pub message_index: usize,
}

/// Separates the client-visible logical chain from the upstream physical chain.
#[derive(Debug, Clone)]
pub struct InteractionStore {
    pub clients: HashMap<String, ClientInteractionNode>,
    pub upstreams: HashMap<String, UpstreamInteractionNode>,
    /// message_hash → client interaction positions.
    /// Multi-valued: duplicate content and branch collisions are valid.
    pub hash_index: HashMap<u64, Vec<ClientInteractionPosition>>,
    /// upstream_id → client ids referencing it (derived index).
    pub upstream_to_clients: HashMap<String, Vec<String>>,
    /// prev_id → child client_id (reverse index for O(1) chain traversal).
    pub prev_to_client: HashMap<String, String>,
}

/// Result of frontier selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontier {
    /// Index into the incoming harness hashes where new messages start.
    pub index: usize,
    /// The previous interaction id to use (None = new chain).
    pub previous_interaction_id: Option<String>,
    /// When true, all incoming messages are known and the handler
    /// should replay rather than POST.
    pub all_known: bool,
    /// When all_known, the client node to replay from.
    pub matched_client_id: Option<String>,
}

impl Default for InteractionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InteractionStore {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            upstreams: HashMap::new(),
            hash_index: HashMap::new(),
            upstream_to_clients: HashMap::new(),
            prev_to_client: HashMap::new(),
        }
    }

    pub fn insert_upstream(&mut self, node: UpstreamInteractionNode) {
        self.upstreams.insert(node.id.clone(), node);
    }

    /// Update last_seen_utc on an upstream interaction node during replay.
    /// Called after every GET fetch that traverses this upstream node.
    pub fn touch_upstream_on_replay(&mut self, upstream_id: &str) {
        if let Some(node) = self.upstreams.get_mut(upstream_id) {
            node.last_seen_utc = unix_now();
        }
    }

    pub fn insert_client(&mut self, node: ClientInteractionNode) {
        // Index every message_hash position
        for (idx, &hash) in node.message_hashes.iter().enumerate() {
            self.hash_index
                .entry(hash)
                .or_default()
                .push(ClientInteractionPosition {
                    client_id: node.id.clone(),
                    message_index: idx,
                });
        }
        // Index upstream_to_clients
        for upstream_id in &node.upstream_ids {
            self.upstream_to_clients
                .entry(upstream_id.clone())
                .or_default()
                .push(node.id.clone());
        }
        // Index prev_to_client for O(1) chain traversal
        if let Some(ref prev_id) = node.prev_id {
            self.prev_to_client.insert(prev_id.clone(), node.id.clone());
        }
        self.clients.insert(node.id.clone(), node);
    }

    pub fn get_client(&self, id: &str) -> Option<&ClientInteractionNode> {
        self.clients.get(id)
    }

    pub fn get_upstream(&self, id: &str) -> Option<&UpstreamInteractionNode> {
        self.upstreams.get(id)
    }

    pub fn lookup_hash(&self, hash: u64) -> &[ClientInteractionPosition] {
        self.hash_index
            .get(&hash)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Walk the client chain from leaf to root, inclusive.
    pub fn walk_client_chain(&self, id: &str) -> Vec<&ClientInteractionNode> {
        let mut result = Vec::new();
        let mut current = self.clients.get(id);
        while let Some(node) = current {
            result.push(node);
            current = node
                .prev_id
                .as_deref()
                .and_then(|pid| self.clients.get(pid));
        }
        result
    }

    /// Walk the upstream chain from leaf to root, inclusive.
    pub fn walk_upstream_chain(&self, id: &str) -> Vec<&UpstreamInteractionNode> {
        let mut result = Vec::new();
        let mut current = self.upstreams.get(id);
        while let Some(node) = current {
            result.push(node);
            current = node
                .prev_id
                .as_deref()
                .and_then(|pid| self.upstreams.get(pid));
        }
        result
    }
}

/// Find the longest valid prefix of incoming harness-message hashes
/// that belongs to one valid client interaction chain in order.
///
/// Algorithm:
/// 1. Look up each hash to get candidates.
/// 2. Validate ordered prefix membership against concrete client nodes.
/// 3. If prefix ends at a client interaction boundary: prev_id = that client's id.
/// 4. If prefix ends inside a client interaction: fork at the client's prev_id.
/// 5. If all messages known and prev_id matches: all_known = true.
/// 6. Tie-break: newest last_seen_utc, then lexicographically smallest id.
pub fn find_frontier(
    hashes: &[u64],
    incoming_prev_id: Option<&str>,
    incoming_system_instruction_hash: Option<u64>,
    store: &InteractionStore,
) -> Frontier {
    if hashes.is_empty() {
        return Frontier {
            index: 0,
            previous_interaction_id: incoming_prev_id.map(String::from),
            all_known: false,
            matched_client_id: None,
        };
    }

    // Build candidate sets for each position
    let candidates: Vec<Vec<ClientInteractionPosition>> = hashes
        .iter()
        .map(|&h| store.lookup_hash(h).to_vec())
        .collect();

    // For each candidate at position 0, try to extend to longest valid prefix
    let mut best: Option<(usize, &ClientInteractionNode, bool)> = None;

    for pos0 in &candidates[0] {
        let client0 = match store.clients.get(&pos0.client_id) {
            Some(c) => c,
            None => continue,
        };

        // Check: does pos0.message_index match within client0?
        if pos0.message_index >= client0.message_hashes.len() {
            continue;
        }
        if client0.message_hashes[pos0.message_index] != hashes[0] {
            continue;
        }

        // Check system_instruction_hash matches root of this candidate's chain
        if let Some(incoming_hash) = incoming_system_instruction_hash {
            let root = find_chain_root(client0, store);
            if root.system_instruction_hash != Some(incoming_hash) {
                // system_instruction changed — skip this candidate, force fork
                continue;
            }
        }

        // Walk forward through the client chain
        let mut prefix_len = 0usize;
        let mut expected_idx = pos0.message_index;
        let mut current_client = client0;
        let mut at_boundary = false;

        for (i, &h) in hashes.iter().enumerate() {
            if expected_idx < current_client.message_hashes.len()
                && current_client.message_hashes[expected_idx] == h
            {
                prefix_len = i + 1;
                expected_idx += 1;
                if expected_idx == current_client.message_hashes.len() {
                    // Reached end of this client node — move to next in chain
                    at_boundary = true;
                    if let Some(next_id) = next_in_chain(current_client, store) {
                        if let Some(next) = store.clients.get(next_id) {
                            current_client = next;
                            expected_idx = 0;
                            at_boundary = false;
                            continue;
                        }
                    }
                    // No next node in chain — any remaining hashes are unknown
                    break;
                }
                at_boundary = false;
            } else {
                // Mismatch at position i within current_client
                break;
            }
        }

        // Determine the termination client node for this prefix
        let terminal_client = if prefix_len == 0 {
            continue;
        } else if at_boundary && expected_idx == 0 {
            // Prefix ended exactly at client boundary, and we moved to next node
            // The terminal client is the one we just finished
            walk_back_from(current_client, store)
        } else {
            // Prefix ended inside a client node
            current_client
        };

        // Proof of ownership: either caller provided explicit prev_id (stateful),
        // or hash-chain match proves identity (stateless, incoming_prev_id = None).
        let is_known = prefix_len == hashes.len()
            && (incoming_prev_id.is_none()
                || incoming_prev_id == terminal_client.prev_id.as_deref());

        match &mut best {
            Some((best_len, best_node, best_known)) => {
                if prefix_len > *best_len {
                    *best_len = prefix_len;
                    *best_node = terminal_client;
                    *best_known = is_known;
                } else if prefix_len == *best_len {
                    // Tie-break: newest last_seen_utc, then lexicographically smallest id
                    let best_utc = best_node.last_seen_utc;
                    let cur_utc = terminal_client.last_seen_utc;
                    if cur_utc > best_utc
                        || (cur_utc == best_utc && terminal_client.id < best_node.id)
                    {
                        *best_node = terminal_client;
                        *best_known = is_known;
                    }
                }
            }
            None => {
                best = Some((prefix_len, terminal_client, is_known));
            }
        }
    }

    match best {
        Some((prefix_len, terminal_client, all_known)) => {
            if all_known {
                Frontier {
                    index: prefix_len,
                    previous_interaction_id: terminal_client.prev_id.clone(),
                    all_known: true,
                    matched_client_id: Some(terminal_client.id.clone()),
                }
            } else if prefix_len == 0 {
                Frontier {
                    index: 0,
                    previous_interaction_id: None,
                    all_known: false,
                    matched_client_id: None,
                }
            } else {
                // Check if we ended at a boundary or inside
                let ended_at_boundary =
                    ended_at_client_boundary(hashes, prefix_len, terminal_client, store);
                if ended_at_boundary {
                    Frontier {
                        index: prefix_len,
                        previous_interaction_id: Some(terminal_client.id.clone()),
                        all_known: false,
                        matched_client_id: None,
                    }
                } else {
                    // Fork at parent
                    Frontier {
                        index: 0,
                        previous_interaction_id: terminal_client.prev_id.clone(),
                        all_known: false,
                        matched_client_id: None,
                    }
                }
            }
        }
        None => Frontier {
            index: 0,
            previous_interaction_id: None,
            all_known: false,
            matched_client_id: None,
        },
    }
}

/// Find the next client node in chain (child of `node`).
fn next_in_chain<'a>(node: &ClientInteractionNode, store: &'a InteractionStore) -> Option<&'a str> {
    store.prev_to_client.get(&node.id).map(|s| s.as_str())
}

/// Walk backward from `client` along prev_id to find the chain root.
fn find_chain_root<'a>(
    client: &'a ClientInteractionNode,
    store: &'a InteractionStore,
) -> &'a ClientInteractionNode {
    let mut current = client;
    while let Some(prev_id) = &current.prev_id {
        match store.clients.get(prev_id) {
            Some(prev) => current = prev,
            None => break,
        }
    }
    current
}

/// Walk backward from a node to find the node whose prev_id matches.
fn walk_back_from<'a>(
    node: &'a ClientInteractionNode,
    store: &'a InteractionStore,
) -> &'a ClientInteractionNode {
    node.prev_id
        .as_deref()
        .and_then(|pid| store.clients.get(pid))
        .unwrap_or(node)
}

/// Check if the prefix ended exactly at a client node boundary.
fn ended_at_client_boundary(
    hashes: &[u64],
    prefix_len: usize,
    terminal_client: &ClientInteractionNode,
    store: &InteractionStore,
) -> bool {
    // Walk back: find the client node whose message_hashes contain hashes[prefix_len-1]
    let last_hash = hashes[prefix_len - 1];
    let mut containing_client = terminal_client;

    // Find which client node actually contains the last matched hash
    let mut current = Some(containing_client);
    while let Some(node) = current {
        if node.message_hashes.contains(&last_hash) {
            containing_client = node;
            break;
        }
        current = node
            .prev_id
            .as_deref()
            .and_then(|pid| store.clients.get(pid));
    }

    // Check if the last hash is the LAST hash in its client node
    containing_client
        .message_hashes
        .last()
        .map(|&h| h == last_hash)
        .unwrap_or(false)
}

/// Versioned persistence document (TOML format).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreDocumentV2 {
    version: u32,
    #[serde(default)]
    sessions: HashMap<String, SessionInfo>,
    #[serde(default)]
    interactions: InteractionsSection,
    #[serde(default)]
    in_flight: HashMap<String, InFlightBatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InteractionsSection {
    #[serde(default)]
    clients: HashMap<String, ClientInteractionNode>,
    #[serde(default)]
    upstreams: HashMap<String, UpstreamInteractionNode>,
}

/// Unified store holding all v2 state.
pub struct StoreV2 {
    pub sessions: HashMap<String, SessionInfo>,
    pub interactions: InteractionStore,
    pub in_flight: HashMap<String, InFlightBatch>,
}

impl Default for StoreV2 {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreV2 {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            interactions: InteractionStore::new(),
            in_flight: HashMap::new(),
        }
    }

    /// Load from a TOML file. Returns v1 entries (ignored) and v2 store if present.
    pub async fn load_from_disk(path: &std::path::Path) -> Result<StoreV2, String> {
        if !path.exists() {
            return Ok(StoreV2::new());
        }
        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| e.to_string())?;

        // Detect old v1 format (no version=2 field)
        let check: toml::Value =
            toml::from_str(&raw).map_err(|e| format!("failed to parse session TOML: {e}"))?;
        if check
            .get("version")
            .and_then(|v| v.as_integer())
            .is_none_or(|v| v != 2)
        {
            tracing::warn!(
                path = %path.display(),
                "old session store format detected (missing version=2), ignoring. count-based sessions cannot be migrated to v2 hashed chains",
            );
            return Ok(StoreV2::new());
        }

        let doc: StoreDocumentV2 =
            toml::from_str(&raw).map_err(|e| format!("failed to deserialize v2 store: {e}"))?;

        let mut store = StoreV2 {
            sessions: doc.sessions,
            interactions: InteractionStore {
                clients: doc.interactions.clients,
                upstreams: doc.interactions.upstreams,
                hash_index: HashMap::new(),
                upstream_to_clients: HashMap::new(),
                prev_to_client: HashMap::new(),
            },
            in_flight: doc.in_flight,
        };

        // Rebuild derived indexes from persisted client nodes
        let clients = std::mem::take(&mut store.interactions.clients);
        for node in clients.values() {
            for (idx, &hash) in node.message_hashes.iter().enumerate() {
                store.interactions.hash_index.entry(hash).or_default().push(
                    ClientInteractionPosition {
                        client_id: node.id.clone(),
                        message_index: idx,
                    },
                );
            }
            for upstream_id in &node.upstream_ids {
                store
                    .interactions
                    .upstream_to_clients
                    .entry(upstream_id.clone())
                    .or_default()
                    .push(node.id.clone());
            }
            // Rebuild prev_to_client reverse index
            if let Some(ref prev_id) = node.prev_id {
                store
                    .interactions
                    .prev_to_client
                    .insert(prev_id.clone(), node.id.clone());
            }
        }
        store.interactions.clients = clients;

        tracing::info!(
            sessions = store.sessions.len(),
            client_nodes = store.interactions.clients.len(),
            upstream_nodes = store.interactions.upstreams.len(),
            in_flight_batches = store.in_flight.len(),
            "loaded v2 session store"
        );

        Ok(store)
    }

    /// Persist to TOML file atomically.
    pub async fn save_to_disk(&self, path: &std::path::Path) -> Result<(), String> {
        let doc = StoreDocumentV2 {
            version: 2,
            sessions: self.sessions.clone(),
            interactions: InteractionsSection {
                clients: self.interactions.clients.clone(),
                upstreams: self.interactions.upstreams.clone(),
            },
            in_flight: self.in_flight.clone(),
        };
        let toml_str =
            toml::to_string(&doc).map_err(|e| format!("failed to serialize v2 store: {e}"))?;
        let tmp = path.with_extension("tmp");
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| e.to_string())?;
        }
        tokio::fs::write(&tmp, &toml_str)
            .await
            .map_err(|e| e.to_string())?;
        tokio::fs::rename(&tmp, path)
            .await
            .map_err(|e| e.to_string())
    }

    // ── InFlightStore operations ─────────────────────────────────

    /// Find an in-flight batch matching session + prev_id + message_hashes.
    pub fn find_matching_batch(
        &self,
        session_id: &str,
        prev_interaction_id: Option<&str>,
        message_hashes: &[u64],
    ) -> Option<&InFlightBatch> {
        self.in_flight.values().find(|b| {
            b.session_id == session_id
                && b.prev_interaction_id.as_deref() == prev_interaction_id
                && b.message_hashes == message_hashes
        })
    }

    /// Create a new in-flight batch.
    pub fn create_batch(
        &mut self,
        id: String,
        session_id: String,
        prev_interaction_id: Option<String>,
        message_hashes: Vec<u64>,
        system_instruction_hash: Option<u64>,
        pieces: Vec<InFlightPiece>,
    ) {
        let now = unix_now();
        self.in_flight.insert(
            id.clone(),
            InFlightBatch {
                id,
                session_id,
                prev_interaction_id,
                message_hashes,
                system_instruction_hash,
                pieces,
                created_utc: now,
                updated_utc: now,
            },
        );
    }

    /// Transition a piece from Pending to ResponseStarted.
    pub fn mark_response_started(
        &mut self,
        batch_id: &str,
        piece_index: usize,
    ) -> Result<(), String> {
        let batch = self
            .in_flight
            .get_mut(batch_id)
            .ok_or_else(|| format!("batch {batch_id} not found"))?;
        let piece = batch
            .pieces
            .get_mut(piece_index)
            .ok_or_else(|| format!("piece {piece_index} not found"))?;
        if !matches!(piece.status, InFlightStatus::Pending) {
            return Err(format!(
                "piece {piece_index} is not Pending (currently {:?})",
                piece.status
            ));
        }
        piece.status = InFlightStatus::ResponseStarted;
        batch.updated_utc = unix_now();
        Ok(())
    }

    /// Transition a piece from ResponseStarted to Sent.
    pub fn mark_sent(
        &mut self,
        batch_id: &str,
        piece_index: usize,
        interaction_id: String,
    ) -> Result<(), String> {
        let batch = self
            .in_flight
            .get_mut(batch_id)
            .ok_or_else(|| format!("batch {batch_id} not found"))?;
        let piece = batch
            .pieces
            .get_mut(piece_index)
            .ok_or_else(|| format!("piece {piece_index} not found"))?;
        if !matches!(piece.status, InFlightStatus::ResponseStarted) {
            return Err(format!(
                "piece {piece_index} is not ResponseStarted (currently {:?})",
                piece.status
            ));
        }
        piece.status = InFlightStatus::Sent { interaction_id };
        batch.updated_utc = unix_now();
        Ok(())
    }

    /// Transition a piece to Acked.
    pub fn ack_piece(
        &mut self,
        batch_id: &str,
        piece_index: usize,
        interaction_id: String,
    ) -> Result<(), String> {
        let batch = self
            .in_flight
            .get_mut(batch_id)
            .ok_or_else(|| format!("batch {batch_id} not found"))?;
        let piece = batch
            .pieces
            .get_mut(piece_index)
            .ok_or_else(|| format!("piece {piece_index} not found"))?;
        match &piece.status {
            InFlightStatus::Sent { interaction_id: _ } | InFlightStatus::ResponseStarted => {}
            other => {
                return Err(format!(
                    "piece {piece_index} cannot transition to Acked from {:?}",
                    other
                ));
            }
        }
        piece.status = InFlightStatus::Acked { interaction_id };
        batch.updated_utc = unix_now();
        Ok(())
    }

    /// Check if all pieces in a batch are Acked.
    pub fn batch_is_complete(&self, batch_id: &str) -> bool {
        self.in_flight
            .get(batch_id)
            .map(|b| {
                b.pieces
                    .iter()
                    .all(|p| matches!(p.status, InFlightStatus::Acked { .. }))
            })
            .unwrap_or(false)
    }

    /// Complete a batch: insert UpstreamInteractionNodes + ClientInteractionNode,
    /// then remove the batch.
    pub fn complete_batch(&mut self, batch_id: &str) -> Result<ClientInteractionNode, String> {
        let batch = self
            .in_flight
            .remove(batch_id)
            .ok_or_else(|| format!("batch {batch_id} not found"))?;

        let now = unix_now();
        // Insert UpstreamInteractionNodes in chain order
        let mut upstream_ids = Vec::with_capacity(batch.pieces.len());
        let mut prev_upstream = batch.prev_interaction_id.clone();
        for piece in &batch.pieces {
            match &piece.status {
                InFlightStatus::Acked { interaction_id } => {
                    self.interactions.insert_upstream(UpstreamInteractionNode {
                        id: interaction_id.clone(),
                        prev_id: prev_upstream.clone(),
                        client_id: format!("{}:chunk-{}", batch.id, piece.index),
                        last_seen_utc: now,
                        expires_at_utc: now + DEFAULT_SESSION_TTL_SECS,
                    });
                    upstream_ids.push(interaction_id.clone());
                    prev_upstream = Some(interaction_id.clone());
                }
                _ => {
                    return Err(format!(
                        "batch {} cannot complete: piece {} is not Acked",
                        batch_id, piece.index
                    ));
                }
            }
        }

        let final_id = upstream_ids.last().cloned().unwrap_or_default();
        let client_node = ClientInteractionNode {
            id: final_id,
            prev_id: batch.prev_interaction_id,
            message_hashes: batch.message_hashes,
            system_instruction_hash: batch.system_instruction_hash,
            upstream_ids,
            last_seen_utc: now,
        };
        self.interactions.insert_client(client_node.clone());

        // Update session metadata
        if let Some(session) = self.sessions.get_mut(&batch.session_id) {
            session.last_interaction_id = Some(client_node.id.clone());
            session.last_seen_utc = now;
        }

        Ok(client_node)
    }

    /// Fail a batch and return the IDs of Acked pieces that need cancelling.
    pub fn fail_batch(&mut self, batch_id: &str, error: String) -> Result<Vec<String>, String> {
        let batch = self
            .in_flight
            .get_mut(batch_id)
            .ok_or_else(|| format!("batch {batch_id} not found"))?;

        let acked_ids: Vec<String> = batch
            .pieces
            .iter()
            .filter_map(|p| match &p.status {
                InFlightStatus::Acked { interaction_id } => Some(interaction_id.clone()),
                _ => None,
            })
            .collect();

        // Remove upstream nodes for Acked pieces
        for id in &acked_ids {
            self.interactions.upstreams.remove(id);
            self.interactions.upstream_to_clients.remove(id);
        }

        // Mark all non-Acked pieces as Failed
        for piece in &mut batch.pieces {
            if !matches!(piece.status, InFlightStatus::Acked { .. }) {
                piece.status = InFlightStatus::Failed {
                    error: error.clone(),
                };
            }
        }
        batch.updated_utc = unix_now();

        Ok(acked_ids)
    }

    /// Remove a failed batch from the store.
    pub fn remove_batch(&mut self, batch_id: &str) {
        self.in_flight.remove(batch_id);
    }

    /// Update a piece's request_body before sending.
    pub fn set_piece_body(
        &mut self,
        batch_id: &str,
        piece_index: usize,
        body: Vec<u8>,
    ) -> Result<(), String> {
        let batch = self
            .in_flight
            .get_mut(batch_id)
            .ok_or_else(|| format!("batch {batch_id} not found"))?;
        let piece = batch
            .pieces
            .get_mut(piece_index)
            .ok_or_else(|| format!("piece {piece_index} not found"))?;
        piece.request_body = body;
        batch.updated_utc = unix_now();
        Ok(())
    }

    /// Clear all sessions, interaction nodes, hash_index, upstream_to_clients,
    /// and in-flight batches. Returns the sessions that were cleared (for
    /// upstream interaction cancellation).
    pub fn clean_all(&mut self) -> Vec<(String, SessionInfo)> {
        let sessions: Vec<(String, SessionInfo)> = self.sessions.drain().collect();
        self.interactions.clients.clear();
        self.interactions.upstreams.clear();
        self.interactions.hash_index.clear();
        self.interactions.upstream_to_clients.clear();
        self.in_flight.clear();
        sessions
    }

    /// Remove all in-flight batches without completing them.
    /// Used on startup to discard stale state carried over
    /// from unclean previous shutdown.
    pub fn discard_all_inflight(&mut self) -> usize {
        let count = self.in_flight.len();
        self.in_flight.clear();
        count
    }

    /// Update session and current interaction node last_seen_utc timestamps.
    pub fn extend_lifetime(&mut self, session_id: &str, expires_at_utc: u64) {
        let now = unix_now();
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.expires_at_utc = expires_at_utc;
            session.last_seen_utc = now;
        }
        // Update last_seen_utc on the current client interaction node if present
        if let Some(session) = self.sessions.get(session_id) {
            if let Some(ref last_id) = session.last_interaction_id {
                if let Some(client) = self.interactions.clients.get_mut(last_id) {
                    client.last_seen_utc = now;
                }
            }
        }
    }

    /// Collect all known upstream interaction ids and in-flight acked piece ids
    /// for cancellation/deletion during clean-all.
    pub fn all_upstream_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.interactions.upstreams.keys().cloned().collect();
        // Also include Acked in-flight piece ids
        for batch in self.in_flight.values() {
            for piece in &batch.pieces {
                if let InFlightStatus::Acked { ref interaction_id } = piece.status {
                    ids.push(interaction_id.clone());
                }
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("test-sessions-{name}-{}.toml", std::process::id()))
    }

    // ── InteractionStore and Frontier Selection ──────────

    fn make_client_node(
        id: &str,
        prev_id: Option<&str>,
        hashes: Vec<u64>,
        upstream_ids: Vec<&str>,
    ) -> ClientInteractionNode {
        ClientInteractionNode {
            id: id.to_string(),
            prev_id: prev_id.map(String::from),
            message_hashes: hashes,
            system_instruction_hash: None,
            upstream_ids: upstream_ids.into_iter().map(String::from).collect(),
            last_seen_utc: 100,
        }
    }

    fn make_upstream_node(id: &str, prev_id: Option<&str>) -> UpstreamInteractionNode {
        UpstreamInteractionNode {
            id: id.to_string(),
            prev_id: prev_id.map(String::from),
            client_id: "req-1".to_string(),
            last_seen_utc: 100,
            expires_at_utc: 200,
        }
    }

    #[test]
    fn interaction_store_insert_and_lookup_upstream() {
        let mut store = InteractionStore::new();
        store.insert_upstream(make_upstream_node("int-A", None));
        let node = store
            .get_upstream("int-A")
            .expect("must find upstream node");
        assert_eq!(node.id, "int-A");
        assert!(node.prev_id.is_none());
    }

    #[test]
    fn interaction_store_insert_and_lookup_client() {
        let mut store = InteractionStore::new();
        store.insert_client(make_client_node("int-A", None, vec![0xA], vec!["int-A"]));
        let node = store.get_client("int-A").expect("must find client node");
        assert_eq!(node.id, "int-A");
        assert_eq!(node.message_hashes, vec![0xA]);
        assert_eq!(node.upstream_ids, vec!["int-A"]);
        // Hash index
        let positions = store.lookup_hash(0xA);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].client_id, "int-A");
        assert_eq!(positions[0].message_index, 0);
    }

    #[test]
    fn interaction_store_hash_index_supports_duplicates() {
        let mut store = InteractionStore::new();
        store.insert_client(make_client_node(
            "int-A",
            None,
            vec![0xA, 0xB],
            vec!["int-A"],
        ));
        store.insert_client(make_client_node(
            "int-B",
            None,
            vec![0xA, 0xC],
            vec!["int-B"],
        ));
        let positions = store.lookup_hash(0xA);
        assert_eq!(
            positions.len(),
            2,
            "duplicate hash must return both positions"
        );
        let ids: Vec<&str> = positions.iter().map(|p| p.client_id.as_str()).collect();
        assert!(ids.contains(&"int-A"));
        assert!(ids.contains(&"int-B"));
    }

    #[test]
    fn interaction_store_walk_client_chain() {
        let mut store = InteractionStore::new();
        store.insert_client(make_client_node("C1", None, vec![0x1], vec!["int-1"]));
        store.insert_client(make_client_node("C2", Some("C1"), vec![0x2], vec!["int-2"]));
        store.insert_client(make_client_node("C3", Some("C2"), vec![0x3], vec!["int-3"]));
        let chain = store.walk_client_chain("C3");
        assert_eq!(chain.len(), 3, "chain must have 3 nodes");
        assert_eq!(chain[0].id, "C3");
        assert_eq!(chain[1].id, "C2");
        assert_eq!(chain[2].id, "C1");
    }

    #[test]
    fn frontier_known_prefix_at_client_boundary() {
        let mut store = InteractionStore::new();
        // Chain: C1 {hashes: [0xA]} → C2 {hashes: [0xB]}
        store.insert_client(make_client_node("C1", None, vec![0xA], vec!["int-1"]));
        store.insert_client(make_client_node("C2", Some("C1"), vec![0xB], vec!["int-2"]));

        let frontier = find_frontier(&[0xA, 0xB, 0xC], None, None, &store);
        assert_eq!(frontier.index, 2, "first 2 hashes known");
        assert_eq!(frontier.previous_interaction_id.as_deref(), Some("C2"));
        assert!(!frontier.all_known);
    }

    #[test]
    fn frontier_isolated_later_hash_does_not_move_frontier() {
        let mut store = InteractionStore::new();
        // 0xB is only in an unrelated branch, not in any valid prefix starting from 0xA
        store.insert_client(make_client_node(
            "unrelated",
            None,
            vec![0xB],
            vec!["int-X"],
        ));
        // No chain contains 0xA, so prefix [0xA, 0xB] can't start

        let frontier = find_frontier(&[0xA, 0xB], None, None, &store);
        assert_eq!(frontier.index, 0, "0xA is unknown, must be frontier=0");
        assert!(frontier.previous_interaction_id.is_none());
    }

    #[test]
    fn frontier_inside_client_node_forks_at_parent() {
        let mut store = InteractionStore::new();
        // C1 {hashes: [0xA, 0xB, 0xC]}, prev_id = C0
        store.insert_client(make_client_node("C0", None, vec![], vec!["int-0"]));
        store.insert_client(make_client_node(
            "C1",
            Some("C0"),
            vec![0xA, 0xB, 0xC],
            vec!["int-1"],
        ));

        // Incoming: [0xA, 0xB, 0xD] with incoming prev_id = C0
        let frontier = find_frontier(&[0xA, 0xB, 0xD], Some("C0"), None, &store);
        assert_eq!(frontier.index, 0, "fork — re-send from beginning");
        // Fork at C1's parent = C0
        assert_eq!(
            frontier.previous_interaction_id.as_deref(),
            Some("C0"),
            "must fork at C1's parent"
        );
    }

    #[test]
    fn frontier_inside_multi_node_chain_forks_at_parent() {
        let mut store = InteractionStore::new();
        // C1 {hashes: [0xA]} → C2 {hashes: [0xB, 0xC]}
        store.insert_client(make_client_node("C1", None, vec![0xA], vec!["int-1"]));
        store.insert_client(make_client_node(
            "C2",
            Some("C1"),
            vec![0xB, 0xC],
            vec!["int-2"],
        ));

        // Incoming: [0xA, 0xB, 0xD]
        // 0xA matches C1 boundary, 0xB matches C2 position 0
        // 0xC is absent from incoming (C2 suffix divergence)
        let frontier = find_frontier(&[0xA, 0xB, 0xD], None, None, &store);
        // Expect fork: C2's parent = C1
        assert_eq!(frontier.previous_interaction_id.as_deref(), Some("C1"));
        assert_eq!(
            frontier.index, 0,
            "fork at C1, re-send from position 0 of C2"
        );
    }

    #[test]
    fn frontier_equal_tie_break_deterministic() {
        let mut store = InteractionStore::new();
        // Two validated chains with same prefix but different terminal ids
        let mut node_a = make_client_node("int-A", Some("prev-0"), vec![0x1], vec!["up-A"]);
        node_a.last_seen_utc = 100;
        let mut node_b = make_client_node("int-B", Some("prev-0"), vec![0x1], vec!["up-B"]);
        node_b.last_seen_utc = 100;
        store.insert_client(node_a);
        store.insert_client(node_b);

        let frontier = find_frontier(&[0x1], Some("prev-0"), None, &store);
        // Both have same last_seen_utc, "int-A" < "int-B" lexicographically
        assert_eq!(frontier.previous_interaction_id.as_deref(), Some("prev-0"));
        assert!(frontier.all_known);
        assert_eq!(frontier.matched_client_id.as_deref(), Some("int-A"));
    }

    #[test]
    fn frontier_all_known_single_upstream() {
        let mut store = InteractionStore::new();
        store.insert_client(make_client_node(
            "int-A",
            Some("int-0"),
            vec![0x10],
            vec!["int-A"],
        ));

        let frontier = find_frontier(&[0x10], Some("int-0"), None, &store);
        assert!(frontier.all_known, "all hashes known, must be all_known");
        assert_eq!(frontier.matched_client_id.as_deref(), Some("int-A"));
        assert_eq!(frontier.index, 1);
    }

    #[test]
    fn frontier_all_known_with_multiple_upstream_ids() {
        let mut store = InteractionStore::new();
        store.insert_client(make_client_node(
            "int-B",
            Some("int-0"),
            vec![0xA, 0xB],
            vec!["int-A", "int-B"],
        ));

        let frontier = find_frontier(&[0xA, 0xB], Some("int-0"), None, &store);
        assert!(frontier.all_known);
        assert_eq!(frontier.matched_client_id.as_deref(), Some("int-B"));
    }

    #[test]
    fn frontier_all_known_stateless_non_root() {
        // Stateless client (incoming_prev_id = None) with non-root chain.
        // Hash-chain match alone proves ownership — prev_id check skipped.
        let mut store = InteractionStore::new();
        store.insert_client(make_client_node(
            "int-child",
            Some("int-parent"),
            vec![0xCA, 0xFE],
            vec!["up-A", "up-B"],
        ));

        let frontier = find_frontier(&[0xCA, 0xFE], None, None, &store);
        assert!(
            frontier.all_known,
            "stateless all_known must work for non-root nodes"
        );
        assert_eq!(frontier.matched_client_id.as_deref(), Some("int-child"));
        assert_eq!(
            frontier.previous_interaction_id.as_deref(),
            Some("int-parent")
        );
    }

    // ── Phase 3: Versioned Persistence and SessionInfo ──────────

    #[tokio::test]
    async fn v2_store_round_trip() {
        let path = test_path("v2-roundtrip");
        let _ = fs::remove_file(&path);

        let mut store = StoreV2::new();
        store.sessions.insert(
            "sess-1".to_string(),
            SessionInfo {
                client_session_id: "sess-1".to_string(),
                last_interaction_id: Some("int-A".to_string()),
                last_seen_utc: 100,
                expires_at_utc: 200,
            },
        );
        store
            .interactions
            .insert_client(make_client_node("int-A", None, vec![0xA], vec!["int-A"]));
        store
            .interactions
            .insert_upstream(make_upstream_node("int-A", None));

        store.save_to_disk(&path).await.unwrap();
        assert!(path.exists());

        let loaded = StoreV2::load_from_disk(&path).await.unwrap();
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(
            loaded.sessions["sess-1"].last_interaction_id.as_deref(),
            Some("int-A")
        );
        assert_eq!(loaded.interactions.clients.len(), 1);
        assert_eq!(loaded.interactions.upstreams.len(), 1);
        // Hash index must be rebuilt
        let positions = loaded.interactions.lookup_hash(0xA);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].client_id, "int-A");

        let _ = fs::remove_file(&path);
    }

    #[tokio::test]
    async fn old_v1_file_ignored() {
        let path = test_path("v1-ignored");
        let _ = fs::remove_file(&path);

        // Write old v1 format — just a bare session table
        let old = toml::toml! {
            ["sess-1"]
            interaction_id = "int-old"
            message_count = 5
            last_access_utc = 100
            expires_at_utc = 200
            pending = false
        };
        fs::write(&path, toml::to_string(&old).unwrap()).unwrap();

        // Load via new StoreV2 — should get empty store
        let loaded = StoreV2::load_from_disk(&path).await.unwrap();
        assert!(loaded.sessions.is_empty(), "old v1 file must be ignored");
        assert!(loaded.interactions.clients.is_empty());

        let _ = fs::remove_file(&path);
    }

    #[tokio::test]
    async fn session_info_does_not_drive_frontier() {
        let mut store = InteractionStore::new();
        store.insert_client(make_client_node(
            "int-new",
            None,
            vec![0xA],
            vec!["int-new"],
        ));

        // SessionInfo pointed at int-old — frontier must use InteractionStore, not SessionInfo
        let _info = SessionInfo {
            client_session_id: "sess-1".to_string(),
            last_interaction_id: Some("int-old".to_string()),
            last_seen_utc: 100,
            expires_at_utc: 200,
        };

        let frontier = find_frontier(&[0xA], None, None, &store);
        // Frontier is all_known — matched_client_id comes from InteractionStore
        assert!(frontier.all_known);
        assert_eq!(frontier.matched_client_id.as_deref(), Some("int-new"));
        // SessionInfo.last_interaction_id is irrelevant
        assert_ne!(frontier.matched_client_id.as_deref(), Some("int-old"));
    }

    // ── Phase 4: InFlightStore State Machine ─────────────────────

    fn make_piece(index: usize, status: InFlightStatus) -> InFlightPiece {
        InFlightPiece {
            index,
            content_hash: 0,
            request_body: vec![],
            status,
        }
    }

    fn make_batch(
        id: &str,
        session_id: &str,
        prev_id: Option<&str>,
        hashes: Vec<u64>,
        pieces: Vec<InFlightPiece>,
    ) -> InFlightBatch {
        InFlightBatch {
            id: id.to_string(),
            session_id: session_id.to_string(),
            prev_interaction_id: prev_id.map(String::from),
            message_hashes: hashes,
            system_instruction_hash: None,
            pieces,
            created_utc: 100,
            updated_utc: 100,
        }
    }

    #[test]
    fn inflight_piece_pending_to_acked_transitions() {
        let mut store = StoreV2::new();
        store.in_flight.insert(
            "batch-1".to_string(),
            make_batch(
                "batch-1",
                "sess-1",
                None,
                vec![0xA],
                vec![make_piece(0, InFlightStatus::Pending)],
            ),
        );

        store.mark_response_started("batch-1", 0).unwrap();
        assert!(matches!(
            store.in_flight["batch-1"].pieces[0].status,
            InFlightStatus::ResponseStarted
        ));

        store.mark_sent("batch-1", 0, "int-A".into()).unwrap();
        assert!(matches!(
            store.in_flight["batch-1"].pieces[0].status,
            InFlightStatus::Sent { .. }
        ));

        store.ack_piece("batch-1", 0, "int-A".into()).unwrap();
        assert!(matches!(
            store.in_flight["batch-1"].pieces[0].status,
            InFlightStatus::Acked { .. }
        ));
    }

    #[test]
    fn inflight_complete_batch_inserts_nodes() {
        let mut store = StoreV2::new();
        store.sessions.insert(
            "sess-1".to_string(),
            SessionInfo {
                client_session_id: "sess-1".to_string(),
                last_interaction_id: None,
                last_seen_utc: 0,
                expires_at_utc: 200,
            },
        );
        store.in_flight.insert(
            "batch-1".to_string(),
            make_batch(
                "batch-1",
                "sess-1",
                None,
                vec![0x10],
                vec![
                    InFlightPiece {
                        index: 0,
                        content_hash: 0,
                        request_body: vec![],
                        status: InFlightStatus::Acked {
                            interaction_id: "int-A".into(),
                        },
                    },
                    InFlightPiece {
                        index: 1,
                        content_hash: 0,
                        request_body: vec![],
                        status: InFlightStatus::Acked {
                            interaction_id: "int-B".into(),
                        },
                    },
                ],
            ),
        );

        let client = store.complete_batch("batch-1").unwrap();
        assert_eq!(client.id, "int-B");
        assert_eq!(client.prev_id, None);
        assert_eq!(client.system_instruction_hash, None);
        assert_eq!(client.message_hashes, vec![0x10]);
        assert_eq!(client.upstream_ids, vec!["int-A", "int-B"]);

        // Upstream nodes inserted in chain order
        let up_a = store.interactions.get_upstream("int-A").unwrap();
        assert_eq!(up_a.prev_id, None);
        assert_eq!(up_a.client_id, "batch-1:chunk-0");
        let up_b = store.interactions.get_upstream("int-B").unwrap();
        assert_eq!(up_b.prev_id.as_deref(), Some("int-A"));
        assert_eq!(up_b.client_id, "batch-1:chunk-1");

        // Batch removed
        assert!(store.in_flight.is_empty());

        // Session updated
        assert_eq!(
            store.sessions["sess-1"].last_interaction_id.as_deref(),
            Some("int-B")
        );
    }

    #[test]
    fn inflight_complete_batch_propagates_system_instruction_hash() {
        let mut store = StoreV2::new();
        store.sessions.insert(
            "sess-1".to_string(),
            SessionInfo {
                client_session_id: "sess-1".to_string(),
                last_interaction_id: None,
                last_seen_utc: 0,
                expires_at_utc: 200,
            },
        );
        // Manually construct a batch with system_instruction_hash = Some(0xDEAD)
        store.in_flight.insert(
            "batch-1".to_string(),
            InFlightBatch {
                id: "batch-1".to_string(),
                session_id: "sess-1".to_string(),
                prev_interaction_id: None,
                message_hashes: vec![0xA],
                system_instruction_hash: Some(0xDEAD),
                pieces: vec![InFlightPiece {
                    index: 0,
                    content_hash: 0,
                    request_body: vec![],
                    status: InFlightStatus::Acked {
                        interaction_id: "int-A".into(),
                    },
                }],
                created_utc: 100,
                updated_utc: 100,
            },
        );

        let client = store.complete_batch("batch-1").unwrap();
        assert_eq!(client.system_instruction_hash, Some(0xDEAD));
        assert_eq!(client.id, "int-A");

        let up = store.interactions.get_upstream("int-A").unwrap();
        assert_eq!(up.client_id, "batch-1:chunk-0");
    }

    #[test]
    fn inflight_fail_batch_no_client_node() {
        let mut store = StoreV2::new();
        store.in_flight.insert(
            "batch-1".to_string(),
            make_batch(
                "batch-1",
                "sess-1",
                None,
                vec![0xA],
                vec![
                    InFlightPiece {
                        index: 0,
                        content_hash: 0,
                        request_body: vec![],
                        status: InFlightStatus::Acked {
                            interaction_id: "int-A".into(),
                        },
                    },
                    make_piece(1, InFlightStatus::Pending),
                ],
            ),
        );
        // Also insert upstream node for Acked piece
        store
            .interactions
            .insert_upstream(make_upstream_node("int-A", None));

        let acked = store
            .fail_batch("batch-1", "upstream error".into())
            .unwrap();
        assert_eq!(acked, vec!["int-A"]);
        // Upstream node for int-A removed
        assert!(store.interactions.get_upstream("int-A").is_none());
        // No client node created
        assert!(store.interactions.clients.is_empty());
        // P1 is Failed
        assert!(matches!(
            store.in_flight["batch-1"].pieces[1].status,
            InFlightStatus::Failed { .. }
        ));
    }

    #[test]
    fn inflight_find_matching_batch() {
        let mut store = StoreV2::new();
        store.in_flight.insert(
            "batch-1".to_string(),
            make_batch(
                "batch-1",
                "sess-1",
                Some("int-0"),
                vec![0xA],
                vec![make_piece(0, InFlightStatus::Pending)],
            ),
        );
        store.in_flight.insert(
            "batch-2".to_string(),
            make_batch(
                "batch-2",
                "sess-2",
                None,
                vec![0xB],
                vec![make_piece(0, InFlightStatus::Pending)],
            ),
        );

        let found = store.find_matching_batch("sess-1", Some("int-0"), &[0xA]);
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, "batch-1");

        let not_found = store.find_matching_batch("sess-1", Some("int-0"), &[0xB]);
        assert!(not_found.is_none());
    }

    #[test]
    fn inflight_single_piece_batch() {
        let mut store = StoreV2::new();
        store.sessions.insert(
            "sess-1".to_string(),
            SessionInfo {
                client_session_id: "sess-1".to_string(),
                last_interaction_id: None,
                last_seen_utc: 0,
                expires_at_utc: 200,
            },
        );
        store.in_flight.insert(
            "batch-1".to_string(),
            make_batch(
                "batch-1",
                "sess-1",
                None,
                vec![0xC],
                vec![InFlightPiece {
                    index: 0,
                    content_hash: 0,
                    request_body: vec![],
                    status: InFlightStatus::Acked {
                        interaction_id: "int-A".into(),
                    },
                }],
            ),
        );

        let client = store.complete_batch("batch-1").unwrap();
        assert_eq!(client.id, "int-A");
        assert_eq!(client.upstream_ids, vec!["int-A"]);
        assert_eq!(client.message_hashes, vec![0xC]);
    }

    // ── Phase 8: Startup Recovery and Control Messages ────────────

    /// 8.1 — StoreV2::load_from_disk rebuilds hash_index and upstream_to_clients
    /// from persisted client nodes (which are stored without runtime indexes).
    #[tokio::test]
    async fn startup_rebuilds_derived_indexes() {
        let path = test_path("phase8-rebuild");
        let _ = fs::remove_file(&path);

        let mut store = StoreV2::new();
        store.sessions.insert(
            "sess-1".to_string(),
            SessionInfo {
                client_session_id: "sess-1".to_string(),
                last_interaction_id: Some("int-B".to_string()),
                last_seen_utc: 200,
                expires_at_utc: 300,
            },
        );
        store.interactions.insert_client(make_client_node(
            "int-A",
            None,
            vec![0xA0, 0xB0],
            vec!["int-A"],
        ));
        store.interactions.insert_client(make_client_node(
            "int-B",
            Some("int-A"),
            vec![0xC0],
            vec!["int-B"],
        ));
        store
            .interactions
            .insert_upstream(make_upstream_node("int-A", None));
        store
            .interactions
            .insert_upstream(make_upstream_node("int-B", Some("int-A")));

        store.save_to_disk(&path).await.unwrap();

        // Simulate restart: load into fresh store
        let loaded = StoreV2::load_from_disk(&path).await.unwrap();

        // hash_index rebuilt: lookup each message hash
        assert_eq!(loaded.interactions.lookup_hash(0xA0).len(), 1);
        assert_eq!(loaded.interactions.lookup_hash(0xA0)[0].client_id, "int-A");
        assert_eq!(loaded.interactions.lookup_hash(0xB0).len(), 1);
        assert_eq!(loaded.interactions.lookup_hash(0xB0)[0].client_id, "int-A");
        assert_eq!(loaded.interactions.lookup_hash(0xC0).len(), 1);
        assert_eq!(loaded.interactions.lookup_hash(0xC0)[0].client_id, "int-B");
        // Unknown hash returns empty
        assert!(loaded.interactions.lookup_hash(0xDEAD).is_empty());

        // upstream_to_clients rebuilt
        let upstream_int_a = loaded.interactions.upstream_to_clients.get("int-A");
        assert!(upstream_int_a.is_some());
        let upstream_int_a = upstream_int_a.unwrap();
        assert!(upstream_int_a.contains(&"int-A".to_string()));

        let upstream_int_b = loaded.interactions.upstream_to_clients.get("int-B");
        assert!(upstream_int_b.is_some());
        let upstream_int_b = upstream_int_b.unwrap();
        assert!(upstream_int_b.contains(&"int-B".to_string()));

        let _ = fs::remove_file(&path);
    }

    /// 8.2 — Startup resumes pending in-flight piece: persisted batch
    /// with P0 Acked and P1 Pending (with request_body); reload then
    /// resend P1 using the previous id from P0.
    #[tokio::test]
    async fn startup_resumes_pending_inflight_piece() {
        let path = test_path("phase8-inflight");
        let _ = fs::remove_file(&path);

        let mut store = StoreV2::new();
        store.in_flight.insert(
            "batch-1".to_string(),
            InFlightBatch {
                id: "batch-1".to_string(),
                session_id: "sess-1".to_string(),
                prev_interaction_id: None,
                message_hashes: vec![0x10],
                system_instruction_hash: None,
                pieces: vec![
                    InFlightPiece {
                        index: 0,
                        content_hash: 0,
                        request_body: b"POST chunk0".to_vec(),
                        status: InFlightStatus::Acked {
                            interaction_id: "int-A".to_string(),
                        },
                    },
                    InFlightPiece {
                        index: 1,
                        content_hash: 0,
                        request_body: b"POST chunk1".to_vec(),
                        status: InFlightStatus::Pending,
                    },
                ],
                created_utc: 100,
                updated_utc: 100,
            },
        );
        store.save_to_disk(&path).await.unwrap();

        // Simulate restart: load into fresh store
        let loaded = StoreV2::load_from_disk(&path).await.unwrap();
        assert_eq!(loaded.in_flight.len(), 1);

        let batch = &loaded.in_flight["batch-1"];
        assert_eq!(batch.pieces.len(), 2);

        // P0 is trusted as Acked
        match &batch.pieces[0].status {
            InFlightStatus::Acked { interaction_id } => {
                assert_eq!(interaction_id, "int-A");
            }
            other => panic!("expected Acked, got {other:?}"),
        }

        // P1 is Pending — can be resent with prev_id = "int-A"
        match &batch.pieces[1].status {
            InFlightStatus::Pending => {}
            other => panic!("expected Pending, got {other:?}"),
        }

        // P1 has its request_body for resend
        assert_eq!(batch.pieces[1].request_body, b"POST chunk1");
        // prev_interaction_id for P1 comes from P0's acked id
        assert_eq!(batch.prev_interaction_id, None); // batch-level prev
                                                     // The sender should use "int-A" as prev for P1

        let _ = fs::remove_file(&path);
    }

    /// 8.3 — Clean-all clears all new stores: sessions, interaction nodes,
    /// hash index, reverse upstream index, and in-flight batches.
    #[tokio::test]
    async fn clean_all_clears_v2_stores() {
        let mut store = StoreV2::new();
        store.sessions.insert(
            "sess-1".to_string(),
            SessionInfo {
                client_session_id: "sess-1".to_string(),
                last_interaction_id: Some("int-A".to_string()),
                last_seen_utc: 100,
                expires_at_utc: 200,
            },
        );
        store.interactions.insert_client(make_client_node(
            "int-A",
            None,
            vec![0xAA],
            vec!["int-A"],
        ));
        store
            .interactions
            .insert_upstream(make_upstream_node("int-A", None));
        store.in_flight.insert(
            "batch-1".to_string(),
            make_batch(
                "batch-1",
                "sess-1",
                None,
                vec![0xAA],
                vec![make_piece(0, InFlightStatus::Pending)],
            ),
        );

        // Verify pre-conditions
        assert_eq!(store.sessions.len(), 1);
        assert_eq!(store.interactions.clients.len(), 1);
        assert_eq!(store.interactions.upstreams.len(), 1);
        assert_eq!(store.in_flight.len(), 1);
        assert_eq!(store.interactions.hash_index.len(), 1);
        assert_eq!(store.interactions.upstream_to_clients.len(), 1);

        // Clean-all
        store.clean_all();

        // All stores must be empty
        assert!(store.sessions.is_empty());
        assert!(store.interactions.clients.is_empty());
        assert!(store.interactions.upstreams.is_empty());
        assert!(store.in_flight.is_empty());
        assert!(store.interactions.hash_index.is_empty());
        assert!(store.interactions.upstream_to_clients.is_empty());
    }

    /// 8.4 — Extend-lifetime updates SessionInfo metadata and current
    /// interaction node's last_seen_utc.
    #[tokio::test]
    async fn extend_lifetime_updates_v2_session_and_client_node() {
        let mut store = StoreV2::new();
        let new_expiry = 999_999;

        store.sessions.insert(
            "sess-1".to_string(),
            SessionInfo {
                client_session_id: "sess-1".to_string(),
                last_interaction_id: Some("int-A".to_string()),
                last_seen_utc: 100,
                expires_at_utc: 200,
            },
        );
        store.interactions.insert_client(make_client_node(
            "int-A",
            None,
            vec![0xBB],
            vec!["int-A"],
        ));

        store.extend_lifetime("sess-1", new_expiry);

        // SessionInfo updated
        let session = &store.sessions["sess-1"];
        assert_eq!(session.expires_at_utc, new_expiry);
        assert!(session.last_seen_utc > 200, "last_seen_utc must be updated");

        // Current interaction node's last_seen_utc updated
        let client_node = &store.interactions.clients["int-A"];
        assert!(client_node.last_seen_utc > 0);
    }

    // ── Startup cleanup tests ────────────────────────────────────

    #[tokio::test]
    async fn startup_discards_non_fully_acked_batches() {
        let path = test_path("startup-discard");

        // Persist v2 store with two batches: one fully-acked, one with Pending piece
        {
            let mut store = StoreV2::new();
            store.create_batch(
                "batch-A".into(),
                "s1".into(),
                None,
                vec![0xAA],
                None,
                vec![InFlightPiece {
                    index: 0,
                    content_hash: 0,
                    request_body: vec![],
                    status: InFlightStatus::Acked {
                        interaction_id: "int-1".into(),
                    },
                }],
            );
            store.create_batch(
                "batch-B".into(),
                "s2".into(),
                None,
                vec![0xBB],
                None,
                vec![InFlightPiece {
                    index: 0,
                    content_hash: 0,
                    request_body: vec![],
                    status: InFlightStatus::Pending,
                }],
            );
            store.save_to_disk(&path).await.unwrap();
        }

        // Act: startup cleanup (simulate load + two-pass)
        let mut store = StoreV2::load_from_disk(&path).await.unwrap();

        // Pass 1: complete fully-acked
        let batch_ids: Vec<String> = store.in_flight.keys().cloned().collect();
        let mut completed = 0usize;
        for batch_id in &batch_ids {
            let all_acked = store.in_flight.get(batch_id).is_some_and(|b| {
                b.pieces
                    .iter()
                    .all(|p| matches!(p.status, InFlightStatus::Acked { .. }))
            });
            if all_acked {
                let _ = store.complete_batch(batch_id);
                completed += 1;
            }
        }
        assert_eq!(completed, 1, "batch-A must be completed");

        // Pass 2: discard rest (batch-B still present after batch-A completed)
        let discarded = store.discard_all_inflight();
        assert_eq!(discarded, 1, "batch-B must be discarded");

        // Assert: all in-flight removed
        assert!(
            store.in_flight.is_empty(),
            "all in-flight batches must be removed after startup cleanup"
        );
        // batch-A completed → ClientInteractionNode created
        assert!(
            store.interactions.clients.contains_key("int-1"),
            "fully-acked batch-A must produce ClientInteractionNode"
        );
        // batch-B was discarded (not completed) → no ClientInteractionNode for it
        // batch-B had Pending piece, never produced an upstream interaction_id
    }

    #[tokio::test]
    async fn startup_cleanup_removes_failed_batches() {
        let path = test_path("startup-failed");

        {
            let mut store = StoreV2::new();
            store.create_batch(
                "batch-F".into(),
                "s1".into(),
                None,
                vec![0xCC],
                None,
                vec![InFlightPiece {
                    index: 0,
                    content_hash: 0,
                    request_body: vec![],
                    status: InFlightStatus::Failed {
                        error: "timeout".into(),
                    },
                }],
            );
            store.save_to_disk(&path).await.unwrap();
        }

        let mut store = StoreV2::load_from_disk(&path).await.unwrap();

        // Pass 1: nothing to complete (Failed != Acked)
        let batch_ids: Vec<String> = store.in_flight.keys().cloned().collect();
        for batch_id in &batch_ids {
            let all_acked = store.in_flight.get(batch_id).is_some_and(|b| {
                b.pieces
                    .iter()
                    .all(|p| matches!(p.status, InFlightStatus::Acked { .. }))
            });
            assert!(!all_acked, "Failed batch must not be all-acked");
        }

        // Pass 2: discard
        let discarded = store.discard_all_inflight();
        assert_eq!(discarded, 1, "failed batch must be discarded");
        assert!(
            store.in_flight.is_empty(),
            "failed batches must be discarded on startup"
        );
    }

    #[tokio::test]
    async fn startup_cleanup_does_not_touch_committed_interactions() {
        let path = test_path("startup-committed");

        {
            let mut store = StoreV2::new();
            // Already-committed ClientInteractionNode (not via in_flight)
            store.interactions.insert_client(ClientInteractionNode {
                id: "existing-id".into(),
                prev_id: None,
                message_hashes: vec![0x11],
                system_instruction_hash: None,
                upstream_ids: vec!["up-1".into()],
                last_seen_utc: 1,
            });
            store.sessions.insert(
                "s-ok".into(),
                SessionInfo {
                    client_session_id: "s-ok".into(),
                    last_interaction_id: Some("existing-id".into()),
                    last_seen_utc: 1,
                    expires_at_utc: 999999,
                },
            );
            store.save_to_disk(&path).await.unwrap();
        }

        let mut store = StoreV2::load_from_disk(&path).await.unwrap();

        // Pass 1: no batches
        let batch_ids: Vec<String> = store.in_flight.keys().cloned().collect();
        assert!(batch_ids.is_empty(), "no in-flight batches");

        // Pass 2: discard (no-op)
        let discarded = store.discard_all_inflight();
        assert_eq!(discarded, 0);

        assert!(
            store.interactions.clients.contains_key("existing-id"),
            "committed interactions survive startup cleanup"
        );
        assert!(
            store.sessions.contains_key("s-ok"),
            "committed session metadata survives startup cleanup"
        );
    }

    /// RED: upstream node last_seen_utc is NOT updated on replay fetch.
    /// Spec says "UpstreamInteractionNode.last_seen_utc is updated on
    /// creation and on every GET replay that traverses this node."
    #[tokio::test]
    async fn replay_updates_upstream_node_last_seen_utc() {
        let path =
            std::env::temp_dir().join(format!("test-replay-touch-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);

        // Insert upstream node with old timestamp
        {
            let mut store = StoreV2::new();
            store.interactions.insert_upstream(UpstreamInteractionNode {
                id: "up-1".into(),
                prev_id: None,
                client_id: "session-1".into(),
                last_seen_utc: 100,
                expires_at_utc: 999999,
            });
            store.save_to_disk(&path).await.unwrap();
        }

        // Reload and touch on replay
        let mut store = StoreV2::load_from_disk(&path).await.unwrap();
        store.interactions.touch_upstream_on_replay("up-1");

        // GREEN: last_seen_utc updated by touch_upstream_on_replay
        let node = store.interactions.get_upstream("up-1").unwrap();
        assert!(
            node.last_seen_utc > 100,
            "GREEN: touch_upstream_on_replay updated last_seen_utc from 100 to {}",
            node.last_seen_utc
        );
        let _ = store.save_to_disk(&path).await;
    }

    /// RED: fail_batch removes upstream nodes from upstreams but NOT from
    /// upstream_to_clients. To demonstrate, manually insert a client node
    /// that references the Acked piece's interaction_id, then fail_batch.
    #[test]
    fn fail_batch_cleans_upstream_to_clients_index() {
        let mut store = StoreV2::new();

        // Step 1: complete_batch creates upstream "up-A" + client "cli-A"
        {
            store.create_batch(
                "batch-A".into(),
                "session-1".into(),
                None,
                vec![0xA],
                None,
                vec![InFlightPiece {
                    index: 0,
                    content_hash: 1,
                    request_body: vec![],
                    status: InFlightStatus::Acked {
                        interaction_id: "up-A".into(),
                    },
                }],
            );
            store.complete_batch("batch-A").unwrap();
        }

        // Step 2: create batch-B with single Acked piece (interaction_id="up-B")
        store.create_batch(
            "batch-B".into(),
            "session-1".into(),
            None,
            vec![0xB],
            None,
            vec![InFlightPiece {
                index: 0,
                content_hash: 2,
                request_body: vec![],
                status: InFlightStatus::Acked {
                    interaction_id: "up-B".into(),
                },
            }],
        );

        // Manually insert a client node referencing up-B to simulate
        // a scenario where upstream_to_clients has an entry that
        // fail_batch should clean up but currently doesn't.
        store.interactions.insert_client(ClientInteractionNode {
            id: "orphan-client".into(),
            prev_id: None,
            message_hashes: vec![0xB],
            system_instruction_hash: None,
            upstream_ids: vec!["up-B".into()],
            last_seen_utc: 1,
        });

        // Step 3: fail_batch("batch-B")
        let _acked = store
            .fail_batch("batch-B", "test error".to_string())
            .unwrap();

        // up-B removed from upstreams
        assert!(
            store.interactions.upstreams.get("up-B").is_none(),
            "up-B must be removed from upstreams"
        );
        // up-A still in upstreams
        assert!(
            store.interactions.upstreams.get("up-A").is_some(),
            "up-A must survive in upstreams"
        );
        // up-A still in upstream_to_clients
        assert_eq!(
            store.interactions.upstream_to_clients.get("up-A"),
            Some(&vec!["up-A".to_string()]),
            "up-A entry in upstream_to_clients must survive"
        );
        // GREEN: up-B cleaned from upstream_to_clients by fail_batch
        assert!(
            store.interactions.upstream_to_clients.get("up-B").is_none(),
            "GREEN: fail_batch cleaned upstream_to_clients for up-B"
        );
    }

    /// RED: next_in_chain uses prev_to_client index for O(1) lookup.
    /// Without insert_client populating the index, returns None even for
    /// a valid chain.
    #[test]
    fn next_in_chain_is_constant_time() {
        let mut store = super::InteractionStore::new();
        let now = crate::session::unix_now();

        // Build chain A → B → C
        let node_a = ClientInteractionNode {
            id: "A".into(),
            prev_id: None,
            message_hashes: vec![1],
            system_instruction_hash: Some(0xAAAA),
            upstream_ids: vec!["up-a".into()],
            last_seen_utc: now,
        };
        let node_b = ClientInteractionNode {
            id: "B".into(),
            prev_id: Some("A".into()),
            message_hashes: vec![2],
            system_instruction_hash: None,
            upstream_ids: vec!["up-b".into()],
            last_seen_utc: now,
        };
        let node_c = ClientInteractionNode {
            id: "C".into(),
            prev_id: Some("B".into()),
            message_hashes: vec![3],
            system_instruction_hash: None,
            upstream_ids: vec!["up-c".into()],
            last_seen_utc: now,
        };

        store.insert_client(node_a.clone());
        store.insert_client(node_b.clone());
        store.insert_client(node_c.clone());

        // ── RED: will be None without prev_to_client being populated ──
        let next = super::next_in_chain(&node_b, &store);
        assert_eq!(
            next,
            Some("C"),
            "RED: prev_to_client not populated by insert_client, got {next:?}"
        );
    }

    /// If incoming system_instruction_hash differs from chain root's stored hash,
    /// find_frontier forks (index=0, prev_id=None) instead of extending the chain.
    #[test]
    fn frontier_forks_on_system_instruction_hash_mismatch() {
        let mut store = super::InteractionStore::new();
        let now = crate::session::unix_now();

        // Chain: root → child, root has system_instruction_hash=0xSYS1
        let root = ClientInteractionNode {
            id: "int-root".into(),
            prev_id: None,
            message_hashes: vec![0xA, 0xB],
            system_instruction_hash: Some(0x1111),
            upstream_ids: vec!["up-1".into()],
            last_seen_utc: now,
        };
        let child = ClientInteractionNode {
            id: "int-child".into(),
            prev_id: Some("int-root".into()),
            message_hashes: vec![0xC],
            system_instruction_hash: None,
            upstream_ids: vec!["up-2".into()],
            last_seen_utc: now,
        };

        store.insert_client(root);
        store.insert_client(child);

        // Client sends [0xA, 0xB, 0xC] with different system_instruction_hash
        let frontier = find_frontier(&[0xA, 0xB, 0xC], None, Some(0x2222), &store);

        assert_eq!(
            frontier.index, 0,
            "must fork at index 0, got {}",
            frontier.index
        );
        assert_eq!(
            frontier.previous_interaction_id, None,
            "must fork with prev_id=None, got {:?}",
            frontier.previous_interaction_id
        );
        assert!(!frontier.all_known, "all_known must be false when forking");
        assert_eq!(
            frontier.matched_client_id, None,
            "matched_client_id must be None when forking, got {:?}",
            frontier.matched_client_id
        );
    }
}
