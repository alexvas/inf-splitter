# Implementation Tasks: Every protocol handler records dump events

**Change ID:** `add-interactions-dump-recording`

---

## Phase 1: Core dump recording (`send_and_translate` + `handle_stream_response`)

- [x] 1.1 Add `ingress_body` parameter to `send_and_translate` (replaces `request_size: usize`)
- [x] 1.2 Record ingress request dump (original client body) in `send_and_translate`
- [x] 1.3 Record egress request dump (interactions request body) in `send_and_translate`
- [x] 1.4 Record response dump in non-streaming success path
- [x] 1.5 Record response dump in error path (shared `request_id`)
- [x] 1.6 Add `request_id` parameter to `handle_stream_response`
- [x] 1.7 Buffer streaming response bytes (up to `MAX_STREAMING_DUMP_BYTES`) and record dump on stream completion
- [x] 1.8 Update call sites in `handle_from_anthropic` and `handle_from_openai` to pass `body` instead of `body.len()`

**Quality Gate:**
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes

---

## Phase 2: Split-send paths (`handle_split_send` + `send_split_system_instruction`)

- [x] 2.1 Add ingress body recording in `handle_split_send` (pass `body` from call sites)
- [x] 2.2 Record ingress/egress request dumps per chunk in `handle_split_send`
- [x] 2.3 Record response dump for each chunk (including error path) in `handle_split_send`
- [x] 2.4 Record per-chunk egress request + response dumps in `send_split_system_instruction`
- [x] 2.5 Update `handle_split_send` call sites in `handle_from_anthropic` and `handle_from_openai` to pass ingress body

**Quality Gate:**
- [x] `cargo fmt --check` passes
- [x] `cargo clippy --locked -- -D warnings` passes

---

## Phase 3: Tests

- [x] 3.1 Add test: non-streaming interactions request produces dump file
- [x] 3.2 Add test: streaming interactions request produces dump file (up to buffer limit)
- [x] 3.3 Add test: interactions error response produces dump file
- [x] 3.4 Add test: dump file not created when `dump_mode = "off"`
- [x] 3.5 Add test: dump request_id matches stats request_id

**Quality Gate:**
- [x] `cargo test --locked` passes

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] Ready for `/openspec-archive`
