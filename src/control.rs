//! In-band control message handling for Gemini Interactions sessions.
//!
//! Detects specially-formatted control messages in client requests,
//! strips them before delta computation, and processes them locally
//! (without forwarding to the interactions endpoint).

use std::collections::HashSet;

use crate::session::{SessionState, SessionStore};

/// Result of scanning incoming messages for control commands.
#[derive(Debug)]
pub struct ControlResult {
    /// Messages with control entries removed (for forwarding).
    pub cleaned_messages: Vec<serde_json::Value>,
    /// Number of control messages stripped.
    pub stripped_count: usize,
    /// Action to take (None = just strip, no action needed).
    pub action: Option<ControlAction>,
}

#[derive(Debug)]
pub enum ControlAction {
    /// Clean all sessions for this endpoint.
    CleanAll,
    /// Extend current session lifetime to the given UTC timestamp.
    ExtendLifetime(u64),
}

/// Scan messages for control constants. Control constants may contain
/// `<unix_utc>` placeholder — the actual timestamp replaces it in the message.
pub fn scan_control_messages(
    messages: &[serde_json::Value],
    clean_all_constant: Option<&str>,
    extend_lifetime_constant: Option<&str>,
    processed_hashes: &mut HashSet<u64>,
) -> ControlResult {
    let mut cleaned = Vec::with_capacity(messages.len());
    let mut stripped = 0usize;
    let mut action: Option<ControlAction> = None;

    for msg in messages {
        let text = message_text(msg);

        // Check clean-all (exact substring match)
        if let Some(constant) = clean_all_constant {
            if let Some(ref t) = text {
                if t.contains(constant) {
                    let hash = hash_str(t);
                    if !processed_hashes.contains(&hash) {
                        processed_hashes.insert(hash);
                        action = Some(ControlAction::CleanAll);
                    }
                    stripped += 1;
                    continue;
                }
            }
        }

        // Check extend-lifetime (prefix+suffix match around the timestamp)
        if let Some(constant) = extend_lifetime_constant {
            if let Some(ref t) = text {
                if let Some(ts) = match_extend_lifetime(t, constant) {
                    let hash = hash_str(t);
                    if !processed_hashes.contains(&hash) {
                        processed_hashes.insert(hash);
                        action = Some(ControlAction::ExtendLifetime(ts));
                    }
                    stripped += 1;
                    continue;
                }
            }
        }

        cleaned.push(msg.clone());
    }

    ControlResult {
        cleaned_messages: cleaned,
        stripped_count: stripped,
        action,
    }
}

/// Execute a control action.
pub async fn execute_control_action(
    action: &ControlAction,
    session_id: &str,
    store: &SessionStore,
    cancel_fn: impl Fn(&str) -> Result<(), String>,
    delete_fn: impl Fn(&str) -> Result<(), String>,
) -> Result<String, String> {
    match action {
        ControlAction::CleanAll => {
            let all = store.remove_all().await?;
            let mut cancelled = 0usize;
            let mut deleted = 0usize;
            for (sid, state) in &all {
                if !state.interaction_id.is_empty() {
                    // Silently ignore errors — "already gone" is fine
                    let _ = cancel_fn(&state.interaction_id);
                    let _ = delete_fn(&state.interaction_id);
                    cancelled += 1;
                    deleted += 1;
                }
            }
            Ok(format!(
                "Cleaned all {} sessions ({} cancelled, {} deleted)",
                all.len(),
                cancelled,
                deleted
            ))
        }
        ControlAction::ExtendLifetime(until) => {
            store.extend_lifetime(session_id, *until).await?;
            Ok(format!(
                "Session {} lifetime extended to UTC {}",
                session_id, until
            ))
        }
    }
}

/// Extract the text content from a message JSON value.
/// Handles both simple string messages and content-block array messages.
fn message_text(msg: &serde_json::Value) -> Option<String> {
    // Simple string
    if let Some(s) = msg.as_str() {
        return Some(s.to_string());
    }
    // Object with "content" field
    if let Some(content) = msg.get("content") {
        if let Some(s) = content.as_str() {
            return Some(s.to_string());
        }
        if let Some(arr) = content.as_array() {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()).map(String::from))
                .collect();
            if !parts.is_empty() {
                return Some(parts.join(" "));
            }
        }
    }
    None
}

/// Match an extend-lifetime message by splitting the constant on `<unix_utc>`
/// and checking both prefix and suffix. Extracts the timestamp from between them.
fn match_extend_lifetime(text: &str, constant: &str) -> Option<u64> {
    let parts: Vec<&str> = constant.split("<unix_utc>").collect();
    if parts.len() != 2 {
        return None;
    }
    let prefix = parts[0];
    let suffix = parts[1];

    // Find the prefix within the text
    let after_prefix = text.find(prefix).map(|i| &text[i + prefix.len()..])?;
    // Extract digits (the timestamp)
    let digits_end = after_prefix.find(|c: char| !c.is_ascii_digit())?;
    let digits = &after_prefix[..digits_end];
    let remainder = &after_prefix[digits_end..];

    if remainder.starts_with(suffix) {
        digits.parse().ok()
    } else {
        None
    }
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN_ALL: &str = "***!___!--- очисти все сессии gemini interactions ---!___!***";
    const EXTEND_LIFETIME: &str =
        "***!___!--- текущую сессию gemini interactions храни до <unix_utc> ---!___!***";

    fn user_msg(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": text})
    }

    #[test]
    fn scan_detects_clean_all() {
        let msgs = vec![
            user_msg("Hello"),
            user_msg(CLEAN_ALL),
            user_msg("World"),
        ];
        let mut hashes = HashSet::new();
        let result = scan_control_messages(&msgs, Some(CLEAN_ALL), None, &mut hashes);
        assert_eq!(result.stripped_count, 1);
        assert_eq!(result.cleaned_messages.len(), 2);
        assert!(matches!(result.action, Some(ControlAction::CleanAll)));
    }

    #[test]
    fn scan_detects_extend_lifetime() {
        let ext_msg =
            "***!___!--- текущую сессию gemini interactions храни до 1718571800 ---!___!***";
        let msgs = vec![user_msg(ext_msg)];
        let mut hashes = HashSet::new();
        let result =
            scan_control_messages(&msgs, None, Some(EXTEND_LIFETIME), &mut hashes);
        assert_eq!(result.stripped_count, 1);
        assert!(result.cleaned_messages.is_empty());
        match result.action {
            Some(ControlAction::ExtendLifetime(ts)) => assert_eq!(ts, 1718571800),
            other => panic!("expected ExtendLifetime, got {other:?}"),
        }
    }

    #[test]
    fn scan_no_control_messages_passes_through() {
        let msgs = vec![user_msg("Hello"), user_msg("World")];
        let mut hashes = HashSet::new();
        let result = scan_control_messages(&msgs, Some(CLEAN_ALL), Some(EXTEND_LIFETIME), &mut hashes);
        assert_eq!(result.stripped_count, 0);
        assert_eq!(result.cleaned_messages.len(), 2);
        assert!(result.action.is_none());
    }

    #[test]
    fn scan_control_without_constants_configured_is_noop() {
        let msgs = vec![user_msg(CLEAN_ALL)];
        let mut hashes = HashSet::new();
        let result = scan_control_messages(&msgs, None, None, &mut hashes);
        assert_eq!(result.stripped_count, 0);
        assert_eq!(result.cleaned_messages.len(), 1);
    }

    #[test]
    fn scan_idempotent_skips_repeated_control() {
        let msgs = vec![user_msg(CLEAN_ALL)];
        let mut hashes = HashSet::new();
        let r1 = scan_control_messages(&msgs, Some(CLEAN_ALL), None, &mut hashes);
        assert_eq!(r1.stripped_count, 1);
        // Second pass with same hash set
        let r2 = scan_control_messages(&msgs, Some(CLEAN_ALL), None, &mut hashes);
        assert_eq!(r2.stripped_count, 1); // still stripped
        assert!(r2.action.is_none()); // but action is None (already processed)
    }

    #[test]
    fn extract_timestamp_from_constant() {
        // Exact match
        let text =
            "***!___!--- текущую сессию gemini interactions храни до 1718571800 ---!___!***";
        let result = match_extend_lifetime(text, EXTEND_LIFETIME);
        assert_eq!(result, Some(1718571800));

        // Embedded in larger text
        let text2 =
            "some prefix ***!___!--- текущую сессию gemini interactions храни до 1718571800 ---!___!*** some suffix";
        let result2 = match_extend_lifetime(text2, EXTEND_LIFETIME);
        assert_eq!(result2, Some(1718571800));
    }

    #[test]
    fn extract_timestamp_no_match() {
        let result = match_extend_lifetime("no timestamp here", EXTEND_LIFETIME);
        assert_eq!(result, None);
    }
}
