# Implementation Tasks: Deduplicate Interactions Handler

**Change ID:** `deduplicate-interactions-handler`

---

## Phase 1: Extract Helpers

- [x] 1.1 Extract `handle_control_action` — control action execution + response
- [x] 1.2 Extract `build_fallback_response` — response from last interaction stats
- [x] 1.3 Extract `ok_with_session_header` — 200 OK JSON with session header

---

## Phase 2: Replace Call Sites

- [x] 2.1 Replace control action blocks in `handle_from_anthropic` and `handle_from_openai`
- [x] 2.2 Replace fallback response blocks in `handle_split_send` and `send_split_system_instruction`
- [x] 2.3 Replace session-header-on-OK blocks in 5 locations

---

## Completion Checklist

- [x] All phases complete
- [x] 306 tests pass
- [x] Clippy clean, fmt clean
- [x] Ready for `/openspec-archive`
