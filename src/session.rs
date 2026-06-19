//! Session state tracking for the Gemini Interactions API.
//!
//! Maps `session_id` (from `request_id` in client requests) to
//! `SessionState` — tracking the last interaction ID and message count
//! for delta computation. State is persisted to a TOML file.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    pub async fn load_from_disk(&self) -> Result<Vec<(String, SessionState)>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(&self.path).map_err(|e| e.to_string())?;
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
    pub async fn save_to_disk(&self) -> Result<(), String> {
        let sessions = self.sessions.read().await;
        let toml_str =
            toml::to_string(&*sessions).map_err(|e| format!("failed to serialize sessions: {e}"))?;
        // Atomic write: write to temp file, then rename
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, toml_str).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Get or create a session. Updates last_access_utc.
    pub async fn get_or_create(&self, session_id: &str) -> SessionState {
        let now = unix_now();
        let mut sessions = self.sessions.write().await;
        if let Some(state) = sessions.get_mut(session_id) {
            state.last_access_utc = now;
            state.clone()
        } else {
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
        self.save_to_disk().await
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
    if incoming <= delivered {
        // No new messages — client may have reset or is replaying
        (delivered, delivered)
    } else {
        (delivered, incoming)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
        let (start, new_count) = compute_delta(5, 5);
        assert_eq!(start, 5);
        assert_eq!(new_count, 5);
    }

    #[test]
    fn compute_delta_handles_reset() {
        // Client sends fewer messages than before — context was reset
        let (start, new_count) = compute_delta(5, 2);
        assert_eq!(start, 5); // all messages need to be re-sent
        assert_eq!(new_count, 5); // unchanged
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
}
