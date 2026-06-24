# Implementation Tasks: Fix Interactions Session Integrity and Streaming Correctness

**Change ID:** `fix-interactions-session-and-streaming`

Each step is a RED→GREEN pair: write the test first, watch it fail, then implement.

---

## Step 1: System-instruction split response discard

- [x] 1.1 **RED** — Test: `proxy_limit` low enough to split `system_instruction`, no remaining content chunks. Verify response is a valid `MessageResponse` from the last system-instruction chunk, not an empty fallback.
- [x] 1.2 **GREEN** — Store parsed `Interaction` in `last_interaction` after each system-instruction chunk in `send_split_system_instruction`.

---

## Step 2: Split-send non-atomic session update

- [x] 2.1 **RED** — Test: 3-chunk split-send where chunk 2 fails; verify retry starts from chunk 2 with correct `previous_interaction_id`, not re-sending chunk 1.
- [x] 2.2 **GREEN** — Update `message_count` and `previous_interaction_id` after each successful chunk in `handle_split_send`, not only after all chunks.

---

## Step 3: Chunk size estimation omits previous_interaction_id

- [x] 3.1 **RED** — Test: follow-up interaction with `previous_interaction_id` nearly filling `proxy_limit`; verify serialized chunk body does not exceed limit.
- [x] 3.2 **GREEN** — Include `previous_interaction_id` in the serialized envelope template used by `pack_content_into_chunks` for subsequent chunks.

---

## Step 4: Streaming eager pending clear

- [x] 4.1 **RED** — Test: streaming interactions request; verify session stays `pending = true` while stream is in progress, `message_count` unchanged until stream completes.
- [x] 4.2 **GREEN** — Defer `pending = false` and `message_count`/`interaction_id` update until after the stream completes successfully. Set `pending = true` before the upstream call.

---

## Step 5: Pending session recovery not wired at startup

- [x] 5.1 **RED** — Test: persisted session with `pending = true, interaction_id = "abc123"`; startup calls `get_interaction("abc123")` and recovers state.
- [x] 5.2 **GREEN** — In `build_app`, after loading session store, iterate `pending_sessions()`, call `get_interaction` for each. Clear `pending` for completed interactions, remove sessions for 404.

---

## Step 6: Expired sessions never evicted in production

- [x] 6.1 **RED** — Test: create session with past `expires_at_utc`, trigger eviction, verify session is cancelled/deleted and removed from store.
- [x] 6.2 **GREEN** — Call `evict_expired()` on each new session creation (or a periodic timer). Expired sessions get cancel+delete, then removal from store.

---

## Step 7: cancel_interaction/delete_interaction ignore HTTP status

- [x] 7.1 **RED** — Test: mock upstream returning HTTP 500 for cancel/delete; verify function returns `Err` (not `Ok`).
- [x] 7.2 **GREEN** — Check `response.status().is_success()` after `builder.send().await`. On non-2xx, log `tracing::warn!` and return `Err` with status and body.

---

## Step 8: Split-path error responses omit session headers

- [x] 8.1 **RED** — Test: split-send chunk fails with upstream error; verify error response includes `x-claude-code-session-id` (Anthropic) or `x-request-id` (OpenAI).
- [x] 8.2 **GREEN** — Thread `session_id` and `ingress` through `handle_split_send` and `send_split_system_instruction` error paths; insert session header via `session_header_name` + `headers.insert`.

---

## Step 9: Non-UTF-8 body rejected before dump recording

- [x] 9.1 **RED** — Test: upstream returns binary body; verify a base64 dump event is recorded before `AppError::Internal` is returned.
- [x] 9.2 **GREEN** — In `validate_upstream_body`, on UTF-8 decode failure, encode body as base64, log `tracing::warn!` with the payload, then return `Err`.

---

## Step 10: Duplicate content_block_start for index 0

- [x] 10.1 **RED** — Test: SSE stream with `interaction.created` then `step.start { index: 0 }`; verify only one `content_block_start` for index 0 in translated output.
- [x] 10.2 **GREEN** — Track active block index from `StepStart` events. Skip `content_block_start` from `InteractionCreatedEvent` when a `StepStart` for the same index follows (or vice versa — suppress the duplicate).

---

## Step 11: Hardcoded ContentBlockStop for index 0

- [x] 11.1 **RED** — Test: multi-step stream with `step.start { index: 1 }` → `interaction.completed`; verify `ContentBlockStop` is emitted for index 1, not 0.
- [x] 11.2 **GREEN** — Track last active block index. On `InteractionCompletedEvent`, emit `ContentBlockStop` for the last active index instead of hardcoded 0.

---

## Step 12: max_tokens silently truncated above u32::MAX

- [x] 12.1 **RED** — Test: `max_tokens: 5000000000` in ingress → `max_output_tokens` is `u32::MAX` (not wrapped to 705032704).
- [x] 12.2 **GREEN** — Use `u32::try_from`. On `Err`, clamp to `u32::MAX` and emit `tracing::warn!("max_tokens {} exceeds u32::MAX, clamping", val)`.

---

## Step 13: Health check strips query parameters

- [x] 13.1 **RED** — Test: `endpoint_interactions = "https://api.example.com/v1beta/interactions?key=abc"`; health probe URL preserves `?key=abc`.
- [x] 13.2 **GREEN** — In `router.rs` health check, preserve the query string when building the probe URL from the configured `endpoint_interactions`.

---

## Step 14: extend_lifetime fails with timestamp at end of message

- [x] 14.1 **RED** — Test: message `"extend 1718571800"` (no trailing chars after timestamp); verify timestamp `1718571800` is extracted.
- [x] 14.2 **GREEN** — In `control.rs`, handle `None` from `after_prefix.find(|c: char| !c.is_ascii_digit())` by treating the entire remaining string as the timestamp.

---

## Step 15: Non-split success path missing ingress_response_dump

- [x] 15.1 **RED** — Test: non-split interactions request completes successfully with `dump_mode = "all"`; verify `ingress/response` dump entry exists with the translated body.
- [x] 15.2 **GREEN** — In `send_and_translate` success path, call `guard.ingress_dump()` (or a new `ingress_response_dump` method) with the translated response body after `build_response_from_interaction`.

---

## Quality Gate

- [x] `cargo fmt --check` — clean
- [x] `cargo clippy --locked -- -D warnings` — clean
- [x] `cargo test --locked` — all 366 tests pass

---

## Completion Checklist

- [x] All 15 RED→GREEN steps pass
- [x] All quality gates passed
- [x] Spec deltas align with implemented behavior
- [x] Ready for `/openspec-archive fix-interactions-session-and-streaming`
