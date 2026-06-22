# Implementation Tasks: Fix Interactions Dump and Split Bugs

**Change ID:** `fix-interactions-dump-and-split-bugs`

---

## Phase 1: Header masking (diagnostics.rs) — RED→GREEN

- [x] 1.1 **RED**: Add tests for `is_sensitive_header` ✓
- [x] 1.2 **RED**: Add test for `header_pairs_with_masking` ✓
- [x] 1.3 **RED**: Add test for `mask_header_values` ✓
- [x] 1.4 **GREEN**: Add `is_sensitive_header()`, `header_pairs_with_masking()`, `mask_header_values()` helpers ✓
- [x] 1.5 Use `header_pairs_with_masking` in `Diagnostics::record_request_dump` ✓
- [x] 1.6 Use `mask_header_values` in `Diagnostics::record_response_dump` ✓
- [x] 1.7 Use `header_pairs_with_masking` in `RequestDiagnostics::ingress_dump` ✓
- [x] 1.8 Use `header_pairs_with_masking` in `RequestDiagnostics::egress_dump` ✓
- [x] 1.9 Run tests → green ✓

**Quality Gate:**
- [x] `cargo test -- diagnostics` — 32 tests pass
- [x] `cargo clippy` clean

---

## Phase 2: Egress headers — auth.rs (shared helper) — RED→GREEN

- [x] 2.1 **RED**: Add test for `forward_request_headers_map` with `api_key` — verify `x-api-key` and `authorization` are set from key, client auth stripped, non-auth headers forwarded
- [x] 2.2 **RED**: Add test for `forward_request_headers_map` without `api_key` — verify client auth forwarded as-is, no override
- [x] 2.3 **GREEN**: Add `forward_request_headers_map()` to `auth.rs`
- [x] 2.4 Refactor `forward_request_headers()` to delegate to the new map function
- [x] 2.5 Run tests → green

**Quality Gate:**
- [x] `cargo test -- auth` — new + existing tests pass
- [x] `cargo clippy` clean

---

## Phase 3: Egress headers — interactions_handler.rs — RED→GREEN

- [x] 3.1 **RED**: Add test for `build_interactions_headers_map` with `api_key` — verify `x-goog-api-key` set, client auth stripped, `Api-Revision` and `Content-Type` present, non-auth headers forwarded
- [x] 3.2 **RED**: Add test for `build_interactions_headers_map` without `api_key` — verify client auth forwarded, no `x-goog-api-key`
- [x] 3.3 **GREEN**: Add `build_interactions_headers_map()`
- [x] 3.4 Refactor `build_interactions_headers()` to delegate to the new map function
- [x] 3.5 Fix `send_and_translate` (line 490): compute egress headers map, pass to `egress_dump`
- [x] 3.6 Fix `handle_split_send` (line 889): compute egress headers once, pass to `egress_dump`
- [x] 3.7 Fix `send_split_system_instruction` (lines 1073, 1135): compute egress headers once, pass to both `egress_dump` calls
- [x] 3.8 Run tests → green

**Quality Gate:**
- [x] `cargo test -- build_interactions_headers` — new + existing tests pass
- [x] `cargo clippy` clean

---

## Phase 4: Egress headers — openai.rs + anthropic.rs

- [x] 4.1 Fix `openai.rs:125` (passthrough): compute `forward_request_headers_map`, pass to `egress_dump`
- [x] 4.2 Fix `openai.rs:212` (Anthropic→OpenAI non-streaming): same
- [x] 4.3 Fix `openai.rs:304` (Anthropic→OpenAI streaming): same
- [x] 4.4 Fix `anthropic.rs:80` (OpenAI→Anthropic): compute `forward_request_headers_map`, pass to `egress_dump`
- [x] 4.5 Fix `anthropic.rs:202` (OpenAI→Anthropic non-streaming): same
- [x] 4.6 Fix `anthropic.rs:290` (OpenAI→Anthropic streaming): same

**Quality Gate:**
- [x] `cargo test` passes
- [x] `cargo clippy` clean

---

## Phase 5: Size check fix (interactions_handler.rs)

- [x] 5.1 Fix anthropic-path size check (line 229): `serde_json::to_vec(&contents)` → `serde_json::to_vec(&params)`
- [x] 5.2 Fix openai-path size check (line 423): same change

**Quality Gate:**
- [x] `cargo test` passes
- [x] `cargo clippy` clean

---

## Phase 6: JSON body embedding (diagnostics.rs) — RED→GREEN

- [x] 6.1 **RED**: Add test — `DumpBody::Utf8` with `{"error":{"message":"denied"}}` serializes as embedded JSON object
- [x] 6.2 **RED**: Add test — `DumpBody::Utf8` with `"plain text"` serializes as JSON string
- [x] 6.3 **RED**: Add test — `DumpBody::Utf8` with `""` (empty) serializes as JSON string (parse fails)
- [x] 6.4 **RED**: Add test — `DumpBody::Base64` still serializes as string with `encoding: base64`
- [x] 6.5 **GREEN**: Modify `DumpEvent::serialize` — `Utf8` branch tries `serde_json::from_str::<Value>(v)`
- [x] 6.6 Run tests → green

**Quality Gate:**
- [x] `cargo test -- diagnostics` — new + existing tests pass
- [x] `cargo clippy` clean

---

## Phase 7: Verify split-path egress dumps

- [x] 7.1 Confirm each chunk in `handle_split_send` gets `egress_dump` (already verified — no code changes)
- [x] 7.2 Confirm each chunk in `send_split_system_instruction` gets `egress_dump` (already verified — no code changes)

**Quality Gate:**
- [x] No gaps found — split path coverage is complete

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes
- [x] `cargo test --locked` passes
- [x] Ready for `/openspec-archive`
