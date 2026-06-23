# Implementation Tasks: Interactions Proxy-Limit Diagnostics

**Change ID:** `interactions-proxy-limit-diagnostics`

---

## Phase 1: Early Guard Creation

- [x] 1.1 Create `RequestDiagnostics` guard in `handle_from_anthropic` after model extraction, record `ingress_dump`
- [x] 1.2 Create `RequestDiagnostics` guard in `handle_from_openai` after model extraction, record `ingress_dump`
- [x] 1.3 Thread guard to `send_and_translate` (accept instead of creating internally)
- [x] 1.4 Thread guard to `handle_split_send` (accept instead of creating internally)
- [x] 1.5 Thread guard to `handle_control_action` (accept, call `finish()` on success)
- [x] 1.6 Call `guard.finish_with_error(400, ...)` on `can_split_under_limit` failures, return short message to client

**Quality Gate:**
- [x] `cargo check` passes
- [x] `cargo clippy` passes
- [x] `cargo test` passes

---

## Phase 2: Error Message Enrichment

- [x] 2.1 Add `format_bytes()` helper (B → KiB → MiB)
- [x] 2.2 Add per-field envelope breakdown in error: model, stream, generation_config, tools, previous_interaction_id
- [x] 2.3 Add `tool_size_breakdown()` helper for `Tool::Function` (name, total, description, parameters)
- [x] 2.4 Handle non-Function tool variants (type label + total size)
- [x] 2.5 Append tool breakdown to all three `can_split_under_limit` error paths
- [x] 2.6 Make tool breakdown lazy — computed only when limit is exceeded

**Quality Gate:**
- [x] `cargo check` passes
- [x] `cargo clippy` passes
- [x] `cargo test` passes

---

## Phase 3: Test

- [x] 3.1 Copy `single_request.dump` to `tests/data/`
- [x] 3.2 Extract tools fixture `tests/data/tools_from_dump.json`
- [x] 3.3 Write `can_split_reports_per_tool_breakdown_from_dump` — loads 105 tools, calls `can_split_under_limit` with 100 KiB limit, verifies output format
- [x] 3.4 Verify spot-checks: Agent, Bash, Read in output; KiB units; description/parameters fields

**Quality Gate:**
- [x] Test passes
- [x] Full test suite passes

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] `cargo fmt --check` clean
- [x] `cargo clippy --locked -- -D warnings` clean
- [x] `cargo test --locked` all passing
