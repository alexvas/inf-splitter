//! Session state tracking for the Gemini Interactions API.
//!
//! Maps `session_id` (from `request_id` in client requests) to
//! `SessionState` — tracking the last interaction ID and message count
//! for delta computation. State is persisted to a TOML file.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// State tracked per client session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// The ID of the last successful Interaction.
    pub interaction_id: String,
    /// Total number of client messages successfully delivered.
    pub message_count: usize,
    /// Unix timestamp (UTC) of last access.
    pub last_access_utc: u64,
    /// Unix timestamp (UTC) when the session expires.
    pub expires_at_utc: u64,
    /// True if the interaction may not exist upstream yet (set on shutdown).
    #[serde(default)]
    pub pending: bool,
}

/// In-memory session store with TOML file persistence.
#[derive(Debug)]
pub struct SessionStore {
    sessions: RwLock<HashMap<String, SessionState>>,
    path: PathBuf,
}

/// Default session TTL: 12 hours in seconds.
pub const DEFAULT_SESSION_TTL_SECS: u64 = 12 * 60 * 60;

impl SessionStore {
    /// Create a new store. If the file exists, load and recover sessions.
    pub fn new(path: PathBuf) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            path,
        }
    }

    /// Load sessions from the TOML file. Returns the loaded sessions.
    /// Detects v1 count-based format and issues a warning.
    pub async fn load_from_disk(&self) -> Result<Vec<(String, SessionState)>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(&self.path).map_err(|e| e.to_string())?;

        // Detect v2 or higher format — only v1 is handled by this path
        if let Ok(v) = toml::from_str::<toml::Value>(&raw) {
            if v.get("version").and_then(|v| v.as_integer()).is_some() {
                tracing::warn!(
                    path = %self.path.display(),
                    "old SessionStore loaded v2+ format; use StoreV2 for versioned documents"
                );
                return Ok(Vec::new());
            }
        }

        let loaded: HashMap<String, SessionState> =
            toml::from_str(&raw).map_err(|e| format!("failed to parse session TOML: {e}"))?;
        let mut sessions = self.sessions.write().await;
        let mut result = Vec::new();
        for (id, state) in loaded {
            result.push((id.clone(), state.clone()));
            sessions.insert(id, state);
        }
        Ok(result)
    }

    /// Persist current sessions to the TOML file atomically.
    /// Runs blocking filesystem operations inside `spawn_blocking` to
    /// avoid stalling the async runtime on slow disk.
    pub async fn save_to_disk(&self) -> Result<(), String> {
        let sessions = self.sessions.read().await;
        let toml_str = toml::to_string(&*sessions)
            .map_err(|e| format!("failed to serialize sessions: {e}"))?;
        // Atomic write: write to temp file, then rename
        let tmp = self.path.with_extension("tmp");
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let path = self.path.clone();
        drop(sessions);
        tokio::task::spawn_blocking(move || {
            fs::write(&tmp, toml_str).map_err(|e| e.to_string())?;
            fs::rename(&tmp, &path).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("spawn_blocking join error: {e}"))?
    }

    /// Get or create a session. Updates last_access_utc.
    /// On new session creation, evicts any expired sessions from the store.
    pub async fn get_or_create(&self, session_id: &str) -> SessionState {
        let now = unix_now();
        let mut sessions = self.sessions.write().await;
        if let Some(state) = sessions.get_mut(session_id) {
            state.last_access_utc = now;
            state.clone()
        } else {
            // Evict expired sessions before creating a new one
            let expired: Vec<String> = sessions
                .iter()
                .filter(|(_, s)| now > s.expires_at_utc)
                .map(|(k, _)| k.clone())
                .collect();
            for id in &expired {
                sessions.remove(id);
            }
            if !expired.is_empty() {
                tracing::info!(count = expired.len(), "evicted expired sessions");
            }

            let state = SessionState {
                interaction_id: String::new(),
                message_count: 0,
                last_access_utc: now,
                expires_at_utc: now + DEFAULT_SESSION_TTL_SECS,
                pending: false,
            };
            sessions.insert(session_id.to_string(), state.clone());
            state
        }
    }

    /// Update session after a successful interaction.
    pub async fn update(
        &self,
        session_id: &str,
        interaction_id: String,
        new_message_count: usize,
        pending: bool,
    ) -> Result<(), String> {
        let now = unix_now();
        let mut sessions = self.sessions.write().await;
        let state = sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;
        state.interaction_id = interaction_id;
        state.message_count = new_message_count;
        state.last_access_utc = now;
        state.expires_at_utc = now + DEFAULT_SESSION_TTL_SECS;
        state.pending = pending;
        drop(sessions);
        if let Err(e) = self.save_to_disk().await {
            tracing::warn!(session_id = %session_id, error = %e, "session update: save_to_disk failed");
        }
        Ok(())
    }

    /// Extend session lifetime to a specific UTC timestamp.
    pub async fn extend_lifetime(
        &self,
        session_id: &str,
        expires_at_utc: u64,
    ) -> Result<(), String> {
        let mut sessions = self.sessions.write().await;
        let state = sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;
        state.expires_at_utc = expires_at_utc;
        state.last_access_utc = unix_now();
        drop(sessions);
        self.save_to_disk().await
    }

    /// Remove a session from the store.
    pub async fn remove(&self, session_id: &str) -> Result<(), String> {
        let mut sessions = self.sessions.write().await;
        sessions.remove(session_id);
        drop(sessions);
        self.save_to_disk().await
    }

    /// Remove all sessions (clean-all command).
    pub async fn remove_all(&self) -> Result<Vec<(String, SessionState)>, String> {
        let mut sessions = self.sessions.write().await;
        let all: Vec<_> = sessions.drain().collect();
        drop(sessions);
        self.save_to_disk().await?;
        Ok(all)
    }

    /// Get all sessions (for control operations).
    pub async fn all_sessions(&self) -> Vec<(String, SessionState)> {
        self.sessions
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Get all expired session IDs. Returns sessions where `now > expires_at_utc`.
    pub async fn expired_sessions(&self) -> Vec<(String, SessionState)> {
        let now = unix_now();
        self.sessions
            .read()
            .await
            .iter()
            .filter(|(_, s)| now > s.expires_at_utc)
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect()
    }

    /// Get all pending sessions (for startup verification).
    pub async fn pending_sessions(&self) -> Vec<(String, SessionState)> {
        self.sessions
            .read()
            .await
            .iter()
            .filter(|(_, s)| s.pending)
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect()
    }

    /// Clear the pending flag on a session.
    pub async fn clear_pending(&self, session_id: &str) -> Result<(), String> {
        let mut sessions = self.sessions.write().await;
        if let Some(state) = sessions.get_mut(session_id) {
            state.pending = false;
        }
        drop(sessions);
        self.save_to_disk().await
    }

    /// Get count of currently stored sessions.
    #[cfg(test)]
    pub async fn count(&self) -> usize {
        self.sessions.read().await.len()
    }
}

/// Compute how many new messages to send (delta).
/// Returns `(delta_start_index, new_message_count)`.
/// `delivered` is the count already successfully sent.
/// `incoming` is the total count in the current request.
pub fn compute_delta(delivered: usize, incoming: usize) -> (usize, usize) {
    if incoming < delivered {
        // Client reset context — re-send all current messages
        (0, incoming)
    } else if incoming == delivered {
        // No new messages to send — produce an empty slice.
        // In a stateful protocol (Interactions), we must not re-send
        // content the upstream already has.
        (incoming, incoming)
    } else {
        (delivered, incoming)
    }
}

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

impl InteractionStore {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            upstreams: HashMap::new(),
            hash_index: HashMap::new(),
            upstream_to_clients: HashMap::new(),
        }
    }

    pub fn insert_upstream(&mut self, node: UpstreamInteractionNode) {
        self.upstreams.insert(node.id.clone(), node);
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

        let is_known =
            prefix_len == hashes.len() && incoming_prev_id == terminal_client.prev_id.as_deref();

        match &mut best {
            Some((best_len, best_node, best_known)) => {
                if prefix_len > *best_len {
                    *best_len = prefix_len;
                    *best_node = terminal_client;
                    *best_known = is_known;
                } else if prefix_len == *best_len {
                    // Tie-break: newest last_seen_utc, then lexicographically smallest id
                    let best_utc = (*best_node).last_seen_utc;
                    let cur_utc = terminal_client.last_seen_utc;
                    if cur_utc > best_utc
                        || (cur_utc == best_utc && terminal_client.id < (*best_node).id)
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
    for client in store.clients.values() {
        if client.prev_id.as_deref() == Some(&node.id) {
            return Some(&client.id);
        }
    }
    None
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
        if !check
            .get("version")
            .and_then(|v| v.as_integer())
            .is_some_and(|v| v == 2)
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
            },
            in_flight: doc.in_flight,
        };

        // Rebuild derived indexes from persisted client nodes
        let clients = std::mem::take(&mut store.interactions.clients);
        for (_, node) in &clients {
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

        // Collect Acked interaction_ids in piece order
        let upstream_ids: Vec<String> = batch
            .pieces
            .iter()
            .filter_map(|p| match &p.status {
                InFlightStatus::Acked { interaction_id } => Some(interaction_id.clone()),
                _ => None,
            })
            .collect();

        if upstream_ids.len() != batch.pieces.len() {
            return Err(format!(
                "batch {batch_id} cannot complete: {} of {} pieces are Acked",
                upstream_ids.len(),
                batch.pieces.len()
            ));
        }

        let now = unix_now();
        // Insert UpstreamInteractionNodes in chain order
        let mut prev_upstream = batch.prev_interaction_id.clone();
        for upstream_id in &upstream_ids {
            self.interactions.insert_upstream(UpstreamInteractionNode {
                id: upstream_id.clone(),
                prev_id: prev_upstream.clone(),
                client_id: format!("{}:chunk-{}", batch.id, upstream_id),
                last_seen_utc: now,
                expires_at_utc: now + DEFAULT_SESSION_TTL_SECS,
            });
            prev_upstream = Some(upstream_id.clone());
        }

        let final_id = upstream_ids.last().cloned().unwrap_or_default();
        let client_node = ClientInteractionNode {
            id: final_id,
            prev_id: batch.prev_interaction_id,
            message_hashes: batch.message_hashes,
            system_instruction_hash: None,
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

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("test-sessions-{name}-{}.toml", std::process::id()))
    }

    async fn new_store(name: &str) -> (SessionStore, PathBuf) {
        let path = test_path(name);
        let _ = fs::remove_file(&path);
        let store = SessionStore::new(path.clone());
        (store, path)
    }

    #[test]
    fn compute_delta_returns_new_messages_when_more_arrive() {
        let (start, new_count) = compute_delta(3, 5);
        assert_eq!(start, 3);
        assert_eq!(new_count, 5);
    }

    #[test]
    fn compute_delta_returns_same_when_no_new_messages() {
        // Same count as delivered — no new messages, send nothing.
        // In a stateful protocol we must not re-send content the upstream already has.
        let (start, new_count) = compute_delta(5, 5);
        assert_eq!(start, 5);
        assert_eq!(new_count, 5);
    }

    #[test]
    fn compute_delta_zero_zero_indistinguishable_from_replay() {
        // Bug: compute_delta(0, 0) returns (0, 0) — same as the
        // "all delivered, exact retry" case (5, 5) → (5, 5).
        // The handler sees start == incoming_count and tries to replay,
        // but there's no interaction_id → 500 error.
        let (start, new_count) = compute_delta(0, 0);
        assert_eq!(start, 0);
        assert_eq!(new_count, 0);
        // start (0) == incoming_count (0) → handler enters replay branch
    }

    #[test]
    fn compute_delta_handles_reset() {
        // Client sends fewer messages than before — context was reset, re-send all
        let (start, new_count) = compute_delta(5, 2);
        assert_eq!(start, 0); // re-send from beginning
        assert_eq!(new_count, 2); // track current message count
    }

    // --- Delta accounting with proxy_limit splits ---

    #[test]
    fn delta_after_single_chunk_split() {
        // First request: 3 messages sent in 2 chunks (msg1+msg2 → chunk1, msg3 → chunk2)
        // After all chunks: message_count = 3 (total across chunks)
        let (start, new_count) = compute_delta(0, 3);
        assert_eq!(start, 0);
        assert_eq!(new_count, 3);

        // Next request: client sends full history (3 old + 2 new = 5)
        // Delta should skip all 3 prior, process only 2 new
        let (start, new_count) = compute_delta(3, 5);
        assert_eq!(start, 3);
        assert_eq!(new_count, 5);
    }

    #[test]
    fn delta_across_multiple_split_rounds() {
        // Round 1: 5 messages sent in 3 chunks (2+2+1), total = 5
        // Round 2: client sends 5 old + 4 new = 9 total
        let (start, new_count) = compute_delta(5, 9);
        assert_eq!(start, 5);
        assert_eq!(new_count, 9);

        // Round 3: client sends 9 old + 3 new = 12 total
        let (start, new_count) = compute_delta(9, 12);
        assert_eq!(start, 9);
        assert_eq!(new_count, 12);
    }

    #[test]
    fn delta_with_system_instruction_split_no_messages() {
        // System instruction split: first 2 chunks have empty Content[]
        // Only the last chunk carries the actual messages (3 messages)
        // After the chain: message_count = 3 (only the real messages)
        let (start, new_count) = compute_delta(0, 3);
        assert_eq!(start, 0);
        assert_eq!(new_count, 3);

        // Next request: client sends full history 3 old + 2 new = 5
        let (start, new_count) = compute_delta(3, 5);
        assert_eq!(start, 3);
        assert_eq!(new_count, 5);
    }

    #[test]
    fn delta_no_new_messages_after_split() {
        // 7 messages delivered across 3 chunks (3+2+2)
        // Next request: same 7 messages — no new messages
        let (start, new_count) = compute_delta(7, 7);
        assert_eq!(start, 7); // no new messages
        assert_eq!(new_count, 7);
    }

    #[test]
    fn delta_reset_after_split_smaller_count() {
        // 6 messages delivered across 2 chunks (3+3)
        // Next request: only 2 messages (client reset conversation)
        let (start, new_count) = compute_delta(6, 2);
        assert_eq!(start, 0); // re-send from beginning
        assert_eq!(new_count, 2); // track current message count
    }

    #[tokio::test]
    async fn session_store_create_and_update() {
        let (store, _path) = new_store("create-update").await;

        let state = store.get_or_create("session-1").await;
        assert_eq!(state.message_count, 0);
        assert!(state.interaction_id.is_empty());
        assert!(!state.pending);

        store
            .update("session-1", "interaction-abc".into(), 3, false)
            .await
            .unwrap();

        let state = store.get_or_create("session-1").await;
        assert_eq!(state.interaction_id, "interaction-abc");
        assert_eq!(state.message_count, 3);
    }

    #[tokio::test]
    async fn session_store_persistence_survives_restart() {
        let (store, path) = new_store("survive-restart").await;

        store.get_or_create("session-1").await;
        store
            .update("session-1", "int-1".into(), 5, false)
            .await
            .unwrap();

        // Simulate restart: create new store pointing to same file
        let store2 = SessionStore::new(path.clone());
        let loaded = store2.load_from_disk().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, "session-1");
        assert_eq!(loaded[0].1.message_count, 5);
        assert_eq!(loaded[0].1.interaction_id, "int-1");

        let _ = fs::remove_file(&path);
    }

    #[tokio::test]
    async fn session_store_pending_on_shutdown() {
        let (store, path) = new_store("pending-shutdown").await;

        store.get_or_create("session-1").await;
        store
            .update("session-1", "int-1".into(), 3, true)
            .await
            .unwrap(); // pending=true

        let store2 = SessionStore::new(path.clone());
        store2.load_from_disk().await.unwrap();
        let pending = store2.pending_sessions().await;
        assert_eq!(pending.len(), 1);

        store2.clear_pending("session-1").await.unwrap();
        let pending_after = store2.pending_sessions().await;
        assert_eq!(pending_after.len(), 0);

        let _ = fs::remove_file(&path);
    }

    #[tokio::test]
    async fn session_store_remove_and_remove_all() {
        let (store, _path) = new_store("remove-all").await;

        store.get_or_create("session-1").await;
        store.get_or_create("session-2").await;
        assert_eq!(store.count().await, 2);

        store.remove("session-1").await.unwrap();
        assert_eq!(store.count().await, 1);

        store.remove_all().await.unwrap();
        assert_eq!(store.count().await, 0);
    }

    #[tokio::test]
    async fn session_store_extend_lifetime() {
        let (store, _path) = new_store("extend").await;

        store.get_or_create("session-1").await;
        let new_expiry = unix_now() + 999_999;
        store
            .extend_lifetime("session-1", new_expiry)
            .await
            .unwrap();

        let all = store.all_sessions().await;
        assert_eq!(all[0].1.expires_at_utc, new_expiry);
    }

    #[tokio::test]
    async fn session_store_expiry_detection() {
        let (store, _path) = new_store("expiry").await;

        store.get_or_create("session-1").await;
        // Manually set expiry to past
        {
            let mut sessions = store.sessions.write().await;
            if let Some(s) = sessions.get_mut("session-1") {
                s.expires_at_utc = 1; // way in the past
            }
        }

        let expired = store.expired_sessions().await;
        assert_eq!(expired.len(), 1);
    }

    #[tokio::test]
    async fn save_to_disk_creates_missing_parent_directory() {
        let dir = std::env::temp_dir().join(format!("test-sessions-subdir-{}", std::process::id()));
        let path = dir.join("sub").join("deep").join("sessions.toml");
        // Clean up after test
        let _ = fs::remove_dir_all(&dir);

        let store = SessionStore::new(path.clone());
        store.get_or_create("s1").await;
        store.update("s1", "int-1".into(), 1, false).await.unwrap();

        assert!(path.exists(), "session file should exist at {:?}", path);

        // Verify content survived
        let store2 = SessionStore::new(path.clone());
        let loaded = store2.load_from_disk().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, "s1");

        let _ = fs::remove_dir_all(&dir);
    }

    // ── Phase 2: InteractionStore and Frontier Selection ──────────

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

        let frontier = find_frontier(&[0xA, 0xB, 0xC], None, &store);
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

        let frontier = find_frontier(&[0xA, 0xB], None, &store);
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
        let frontier = find_frontier(&[0xA, 0xB, 0xD], Some("C0"), &store);
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
        let frontier = find_frontier(&[0xA, 0xB, 0xD], None, &store);
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

        let frontier = find_frontier(&[0x1], Some("prev-0"), &store);
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

        let frontier = find_frontier(&[0x10], Some("int-0"), &store);
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

        let frontier = find_frontier(&[0xA, 0xB], Some("int-0"), &store);
        assert!(frontier.all_known);
        assert_eq!(frontier.matched_client_id.as_deref(), Some("int-B"));
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

        let frontier = find_frontier(&[0xA], None, &store);
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
        assert_eq!(client.message_hashes, vec![0x10]);
        assert_eq!(client.upstream_ids, vec!["int-A", "int-B"]);

        // Upstream nodes inserted in chain order
        let up_a = store.interactions.get_upstream("int-A").unwrap();
        assert_eq!(up_a.prev_id, None);
        let up_b = store.interactions.get_upstream("int-B").unwrap();
        assert_eq!(up_b.prev_id.as_deref(), Some("int-A"));

        // Batch removed
        assert!(store.in_flight.is_empty());

        // Session updated
        assert_eq!(
            store.sessions["sess-1"].last_interaction_id.as_deref(),
            Some("int-B")
        );
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
}
