# Delta: Configuration

**Change ID:** `redesign-session-state-model`
**Affects:** `openspec/specs/config.md`, `src/config.rs`, `src/session.rs`

---

## MODIFIED

### Requirement: Global Settings

`interactions_session_store` keeps the same name and path semantics, but the on-disk format changes to a versioned v2 interactions state document.

No new config key is introduced. Default paths remain unchanged.

#### Scenario: Custom store path uses v2 format
- GIVEN `interactions_session_store = "/custom/path/sessions.toml"`
- WHEN the proxy persists interactions state
- THEN `/custom/path/sessions.toml` contains `version = 2` and top-level sections for sessions, interactions, and in-flight batches

#### Scenario: Missing store creates v2 document
- GIVEN no persistence file exists
- WHEN the first interactions state change is saved
- THEN a v2 document is created at the configured path

#### Scenario: Old count-based store is not migrated
- GIVEN configured store path contains old count-based session TOML without `version = 2`
- WHEN config and state are loaded
- THEN startup succeeds
- AND old sessions are ignored with a warning
