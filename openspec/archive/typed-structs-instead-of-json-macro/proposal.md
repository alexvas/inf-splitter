# Proposal: Replace manual json!() with typed structs

**Change ID:** `typed-structs-instead-of-json-macro`
**Created:** 2026-06-21
**Status:** Archived
**Archived:** 2026-06-21
**Superseded by:** `eliminate-raw-json-body`

---

## Problem Statement

4 production code sites construct Interactions API request bodies via manual `serde_json::json!({...})` instead of using the generated `CreateModelInteractionParams` struct (from `schemas/interactions.openapi.json` via `build.rs`). The recent bug (missing `model` field in Interactions requests) was a direct consequence — with a typed struct, the compiler would have enforced the field's presence.

## Proposed Solution

1. Add `Default` derive to generated structs/enums in `build.rs` for ergonomic construction
2. Rewrite `build_request_body()` to construct `CreateModelInteractionParams` directly
3. Extract `build_chunk_request()` helper for the 3 chunk-request sites
4. Replace `json!()` in `interactions_handler.rs` with calls to the typed helper
5. Strengthen the CLAUDE.md rule to explicitly mention generated types

## Scope

### In Scope
- `build.rs`: add Default derive
- `src/interactions.rs`: `build_request_body()` rewrite + `build_chunk_request()` helper
- `src/interactions_handler.rs`: 3 chunk-request sites
- `CLAUDE.md`: rule strengthening

### Out of Scope
- Diagnostic `json!()` sites (free-form metadata by design)
- StreamEvent construction (no public constructors in anyllm_translate)
- Test fixtures (acceptable use of json!())
- Generic Value passthrough manipulation

## Success Criteria
- [ ] All 4 `json!()` sites replaced with typed struct construction
- [ ] `cargo check`, `cargo fmt --check`, `cargo clippy --locked -- -D warnings` pass
- [ ] All 184 existing tests pass
- [ ] CLAUDE.md rule strengthened
