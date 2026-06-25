# Proposal: Fix Interactions Protocol Correctness

**Change ID:** `fix-interactions-protocol-correctness`
**Created:** 2026-06-24
**Status:** Implementation Complete
**Completed:** 2026-06-25

---

## Problem Statement

15 protocol correctness bugs in `InteractionsHandler` and `SessionStore` cause data leaks, duplicate sends, memory exhaustion, broken recovery, and incorrect diagnostics.

## Proposed Solution

Fix each bug individually with a test-first (red-green) approach.

## Scope

### In Scope
- All 15 bugs listed in findings
- Unit tests for each fix
- Integration tests where needed

### Out of Scope
- Architectural refactoring
- Async session persistence rewrite (separate change: `redesign-session-state-model`)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions_handler.rs` | Yes | 13 fixes |
| `session.rs` | Yes | 1 fix (spawn_blocking) |
| `lib.rs` | Minor | Recovery loop handles empty-id guard |
| Tests | Yes | 3 new, 3 updated |

## Success Criteria

- [x] All 15 bugs have failing tests proving the bug
- [x] All 15 bugs have minimal fixes
- [x] `cargo fmt` passes
- [x] `cargo clippy --locked` clean
- [x] `cargo test --locked` all 388 tests green

## Archive Information

**Archived:** 2026-06-25
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — 13 fixes across all 5 phases
- `src/session.rs` — `spawn_blocking` for disk writes

### Specs Updated
- `openspec/specs/protocol-conversion.md` — 12 new requirements, 3 modified
- `openspec/specs/diagnostics.md` — 1 new requirement, 1 modified
