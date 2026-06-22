# Proposal: Fix Interactions Dump and Split Bugs

**Change ID:** `fix-interactions-dump-and-split-bugs`
**Created:** 2026-06-22
**Status:** Implementation Complete
**Completed:** 2026-06-22

---

## Problem Statement

Four bugs in diagnostics and handler egress dump calls that cause incorrect
debug output and incorrect proxy_limit splitting:

### Bug 1: Egress dump shows ingress headers (ALL handlers)

All 10 `egress_dump` calls across all three handlers pass `request_headers`
(the incoming client headers) instead of the actual headers sent upstream:

- `openai.rs`: 3 calls — `forward_request_headers` adds/overrides auth but dump doesn't see it
- `anthropic.rs`: 3 calls — same
- `interactions_handler.rs`: 4 calls — `build_interactions_headers` adds `x-goog-api-key` but dump shows `authorization: Bearer dummy`

### Bug 2: No header masking in dumps

`diagnostics.rs` has zero header redaction.

### Bug 3: proxy_limit size check ignores system_instruction and tools

The size check measures only the `input` ContentList, not the full body.

### Bug 4: Dump body is always a JSON string

`"body":"{\"error\":{...}}"` instead of `"body":{"error":{...}}`.

## Proposed Solution

1. Add `forward_request_headers_map()` to `auth.rs` + `build_interactions_headers_map` to `interactions_handler.rs`
2. Masking at Diagnostics level: `header_pairs_with_masking` + `mask_header_values`
3. Size check: `serde_json::to_vec(&contents)` → `serde_json::to_vec(&params)`
4. JSON embedding: `serde_json::from_str::<Value>(v)` in `DumpEvent::serialize`

## Scope

### In Scope
- `src/auth.rs`: add `forward_request_headers_map`, refactor
- `src/interactions_handler.rs`: add `build_interactions_headers_map`, 4 egress_dump sites, 2 size checks
- `src/openai.rs`: 3 egress_dump sites
- `src/anthropic.rs`: 3 egress_dump sites
- `src/diagnostics.rs`: header masking, JSON body embedding
- All tests pass, clippy clean

---

## Archive Information

**Archived:** 2026-06-22
**Duration:** 1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/auth.rs` — +`forward_request_headers_map`, refactored `forward_request_headers`
- `src/interactions_handler.rs` — +`build_interactions_headers_map`, 4 egress_dump fixes, 2 size check fixes
- `src/openai.rs` — 3 egress_dump fixes
- `src/anthropic.rs` — 3 egress_dump fixes
- `src/diagnostics.rs` — masking helpers (3 funcs), JSON body embedding, 4 masking application sites
- `tests/protocol_conversion.rs` — 3 test updates for embedded JSON body format

### Specs Updated
- `openspec/specs/diagnostics.md` — JSON body embedding, header masking, egress headers, proxy_limit size check

### Quality
- 321 tests pass (230 unit + 28 e2e + 63 integration)
- `cargo fmt` clean
- `cargo clippy --locked -- -D warnings` clean
