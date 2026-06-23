# Delta: Diagnostics

**Change ID:** `fix-interactions-diagnostics-gaps`
**Affects:** `src/interactions_handler.rs`, `src/anthropic.rs`, `src/openai.rs`, `src/diagnostics.rs`, `src/session.rs`

---

## MODIFIED

### Requirement: Egress Dump Uses Actual Upstream Headers (All Handlers)

**Updated:** Response dumps now also carry upstream response headers, not just egress request headers.

`response_dump` in all handlers now receives the actual `HeaderMap` from `reqwest::Response::headers()` instead of `vec![]`. This applies to:

**`interactions_handler.rs` (9 call sites):**
- Non-streaming success path (`send_and_translate`)
- Non-streaming error path (`send_and_translate`)
- Streaming path (`handle_stream_response`, via updated `response_dump_streaming`)
- Split-send per-chunk success and error paths (`handle_split_send`, `send_split_system_instruction`)

**`anthropic.rs` (1 call site):**
- Non-streaming error path — already captures `response_headers` at line 90, now passes it instead of `vec![]`

**`openai.rs` (1 call site):**
- Non-streaming error path — already captures `upstream.headers()` at line 135, now passes it instead of `vec![]`

`response_dump_streaming` signature changes from `(body, status)` to `(body, status, headers)` to accept header pairs.

#### Scenario: Non-streaming response dump contains upstream headers
- GIVEN an interactions request completes successfully
- AND the upstream returns headers `content-type: application/json` and `x-request-id: abc123`
- WHEN the response dump is recorded
- THEN `headers` in the dump entry contains both header pairs (non-empty array)
- AND sensitive headers are masked per existing masking rules

#### Scenario: Streaming response dump contains upstream headers
- GIVEN an interactions streaming request completes
- AND the upstream returns response headers
- WHEN `response_dump_streaming` is called from the spawned task
- THEN `headers` in the dump entry contains the upstream response headers

#### Scenario: Error response dump contains upstream headers
- GIVEN the upstream returns a 429 with header `retry-after: 30`
- WHEN the error is handled and a response dump is recorded
- THEN `headers` in the dump entry contains `retry-after: 30`

#### Scenario: Anthropic handler error path includes response headers
- GIVEN an Anthropic passthrough/conversion request fails with upstream error
- AND the upstream response has headers `content-type: application/json` and `x-request-id: abc123`
- WHEN the error path records a response dump at `anthropic.rs:92`
- THEN `headers` contains the upstream response headers (not `[]`)

#### Scenario: OpenAI handler error path includes response headers
- GIVEN an OpenAI passthrough/conversion request fails with upstream error
- AND the upstream response has headers
- WHEN the error path records a response dump at `openai.rs:137`
- THEN `headers` contains the upstream response headers (not `[]`)

---

### Requirement: RequestDiagnostics Session Guard (v2)

**Updated:** `handle_control_action` error paths now call `guard.finish_with_error()` before propagating the error via `?`, preventing the "diagnostics guard dropped without finish" log.

#### Scenario: Control action clean-all fails
- GIVEN `handle_control_action` is called with `ControlAction::CleanAll`
- AND `session_store.remove_all()` returns an error
- WHEN the error propagates via `?`
- THEN `guard.finish_with_error()` is called BEFORE the `?`
- AND a stats entry is recorded with the error message (e.g., "session clean-all failed: ...")
- AND no "diagnostics guard dropped without finish" error is logged

#### Scenario: Control action extend-lifetime fails
- GIVEN `handle_control_action` is called with `ControlAction::ExtendLifetime(ts)`
- AND `session_store.extend_lifetime()` returns an error
- WHEN the error propagates via `?`
- THEN `guard.finish_with_error()` is called BEFORE the `?`
- AND a stats entry is recorded with the error message
- AND no "diagnostics guard dropped without finish" error is logged

---

## ADDED

### Requirement: Session Store Creates Parent Directory on Save

`SessionStore::save_to_disk` must ensure the parent directory exists before writing the temporary file. It uses `std::fs::create_dir_all` on the parent of `self.path` before `fs::write`.

#### Scenario: First run with missing directory
- GIVEN the session file path is `/var/lib/inf-splitter/interactions-sessions.toml`
- AND the directory `/var/lib/inf-splitter/` does not exist
- WHEN `save_to_disk` is called
- THEN `create_dir_all("/var/lib/inf-splitter/")` creates the directory
- AND the TOML file is written successfully

#### Scenario: Directory already exists
- GIVEN the parent directory already exists
- WHEN `save_to_disk` is called
- THEN `create_dir_all` is a no-op
- AND the file is written normally

---

### Requirement: Session Update Errors Are Logged

`SessionStore::update` logs `save_to_disk` errors internally via `tracing::warn!` instead of returning them to callers. This ensures persistence failures are always surfaced regardless of how callers handle the result. The method signature stays `Result<(), String>` for callers that do inspect the error (e.g., `remove_all`), but the warning is already logged by the time the `Result` propagates.

#### Scenario: update fails to persist
- GIVEN `session_store.update()` is called
- AND `save_to_disk` fails (e.g., disk full)
- WHEN the error occurs
- THEN `tracing::warn!` logs the session ID and error details inside `update`
- AND the `Result::Err` is still returned for callers that want to handle it
