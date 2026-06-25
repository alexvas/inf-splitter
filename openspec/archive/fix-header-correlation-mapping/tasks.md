# Implementation Tasks: Fix Header Correlation Mapping

**Change ID:** `fix-header-correlation-mapping`
**Status:** Implementation Complete

---

## Phase 1: Protocol-Aware forward_request_headers_map

- [x] 1.1 RED — Anthropic maps X-Client-Request-Id → x-claude-code-session-id ✓
- [x] 1.2 RED — OpenAI maps x-claude-code-session-id → X-Client-Request-Id ✓
- [x] 1.3 RED — x-request-id never forwarded to any upstream ✓
- [x] 1.4 GREEN — Add protocol parameter and implement mapping rules ✓
- [x] 1.5 GREEN — Update all call sites (openai.rs, anthropic.rs) ✓

**Quality Gate:** PASSED (392 tests, fmt clean, clippy clean)

---

## Phase 2: Remove Gemini x-claude-code-session-id → X-Client-Request-Id

- [x] 2.1 RED — build_interactions_headers_map does not set X-Client-Request-Id ✓
- [x] 2.2 GREEN — Remove unnecessary mapping ✓

**Quality Gate:** PASSED

---

## Completion Checklist

- [x] All phases complete
- [x] `cargo fmt` passes
- [x] `cargo clippy --locked` clean
- [x] `cargo test --locked` all green (392 tests)
- [x] Ready for `/openspec-archive`
