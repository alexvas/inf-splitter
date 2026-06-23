# Implementation Tasks: Fix Conversion Success Response Dumps

**Change ID:** `fix-anthropic-openai-success-response-dumps`

---

## Phase 1: Regression Tests

- [x] 1.1 Add a failing test for successful non-streaming Anthropic→OpenAI conversion with `dump_mode = "all"` expecting `egress/response`.
- [x] 1.2 Add a failing test for successful non-streaming OpenAI→Anthropic conversion with `dump_mode = "all"` expecting `egress/response`.
- [x] 1.3 Assert response dump bodies are raw upstream responses, not translated client responses.
- [x] 1.4 Assert response dump headers contain upstream response headers.
- [x] 1.5 Add or identify streaming conversion success coverage for response dump finalization in both directions.

**Quality Gate:**
- [x] Targeted tests fail before implementation for missing response dumps.

---

## Phase 2: Non-Streaming Conversion Fixes

- [x] 2.1 In `OpenAiHandler::handle_sync_manual`, read successful OpenAI upstream response as bytes.
- [x] 2.2 Record `guard.response_dump(...)` using those bytes and upstream response headers.
- [x] 2.3 Deserialize `ChatCompletionResponse` from the same bytes before translating to Anthropic.
- [x] 2.4 In `AnthropicHandler::handle_from_openai`, read successful Anthropic upstream response as bytes.
- [x] 2.5 Record `guard.response_dump(...)` using those bytes and upstream response headers.
- [x] 2.6 Deserialize `MessageResponse` from the same bytes before translating to OpenAI.
- [x] 2.7 Preserve existing guard finalization and error handling invariants.

**Quality Gate:**
- [x] Non-streaming regression tests pass.

---

## Phase 3: Streaming Conversion Fixes

- [x] 3.1 In `OpenAiHandler::handle_stream_manual`, capture raw OpenAI upstream SSE bytes up to `MAX_STREAMING_DUMP_BYTES` while translating to Anthropic SSE.
- [x] 3.2 Record `guard.response_dump_streaming(...)` with OpenAI upstream response headers when the translated stream completes.
- [x] 3.3 In `AnthropicHandler::handle_from_openai_stream`, capture raw Anthropic upstream SSE bytes up to `MAX_STREAMING_DUMP_BYTES` while translating to OpenAI SSE.
- [x] 3.4 Record `guard.response_dump_streaming(...)` with Anthropic upstream response headers when the translated stream completes.
- [x] 3.5 Move streaming `guard.finish(...)` to the code path that observes stream completion.
- [x] 3.6 Ensure stream errors and response construction errors finalize the guard before returning.
- [x] 3.7 Add a short comment in the interactions streaming client-disconnect path explaining that no response dump is recorded because the upstream response did not complete.

**Quality Gate:**
- [x] Streaming regression or equivalent coverage passes.
- [x] No `diagnostics guard dropped without finish` log is emitted for normal stream completion.

---

## Phase 4: Verification

- [x] 4.1 Run `cargo fmt --check`.
- [x] 4.2 Run `cargo clippy --locked -- -D warnings`.
- [x] 4.3 Run `cargo test --locked`.
- [x] 4.4 Manually inspect or assert dump output contains all expected directions for both conversion directions.

**Quality Gate:**
- [x] All tests pass.
- [x] Lints are clean.

---

## Completion Checklist

- [x] All phases complete.
- [x] All quality gates passed.
- [ ] Diagnostics spec archived after implementation.
- [x] Ready for `/openspec-archive`.
