# Proposal: Fix Interactions Session Integrity and Streaming Correctness

**Change ID:** `fix-interactions-session-and-streaming`
**Created:** 2026-06-23
**Status:** Draft

---

## Problem Statement

Fifteen bugs were identified in the interactions handler, session store, streaming translation, and related components. These fall into five categories:

1. **Split-send correctness (3 bugs):** System-instruction split responses are discarded; chunk-failure retries duplicate already-accepted content upstream; chunk size estimation omits `previous_interaction_id` from the envelope.
2. **Session state integrity (3 bugs):** Streaming eagerly marks sessions non-pending before the stream completes; pending session recovery is never wired at startup; expired sessions accumulate indefinitely.
3. **Error handling gaps (3 bugs):** `cancel_interaction`/`delete_interaction` ignore HTTP status codes; split-path upstream errors omit session/request headers; non-UTF-8 upstream bodies are rejected before dump recording.
4. **Streaming translation bugs (2 bugs):** Duplicate `content_block_start` events for index 0; hardcoded `ContentBlockStop` for index 0 on completion.
5. **Ingress/edge-case bugs (4 bugs):** `max_tokens` truncation above `u32::MAX`; health checks strip query parameters; `extend_lifetime` fails when timestamp ends the message; non-split success path missing `ingress_response_dump`.

These bugs collectively cause: lost model responses, duplicated upstream content, unrecoverable session state after crashes, stale session accumulation, silent control-operation failures, client-visible streaming artifacts, and missing diagnostics data.

## Proposed Solution

Fix each bug at its root cause, following existing code patterns and respecting the typed-construction, diagnostics-guard, and session-state invariants already established in the codebase.

### Split-Send Correctness

- **System-instruction split response:** Store the parsed `Interaction` in `last_interaction` after each system-instruction chunk completes, so `build_fallback_response` has data to construct a valid response when no content chunks follow.
- **Atomic session update on split-send:** Update `message_count` and `previous_interaction_id` after **each** successful chunk (not only after all chunks), so retries don't re-send already-accepted content. The trade-off is that a partial-success retry will have a different `previous_interaction_id` chain, but no content duplication.
- **Chunk size estimation:** Include `previous_interaction_id` in the serialized envelope used for size measurement in `pack_content_into_chunks`, so subsequent chunks don't silently exceed `proxy_limit`.

### Session State Integrity

- **Streaming pending state:** Keep `pending = true` until the stream completes successfully. Only clear `pending` and update `interaction_id`/`message_count` after the full upstream response is received and validated.
- **Startup pending recovery:** In `build_app`, after loading the session store, iterate `pending_sessions()`, call `get_interaction` for each, and clear `pending`/update state for those that completed, or cancel/delete those that are gone.
- **Expired session eviction:** Call `expired_sessions()` on a periodic timer (or at minimum on each new session creation) and evict them, matching the documented startup behavior.

### Error Handling Gaps

- **cancel/delete status checking:** Check the HTTP status code from `builder.send().await`. On non-2xx, log a warning and treat as failure (return the error to the caller).
- **Split-path session headers:** Thread the session header through `handle_split_send` and `send_split_system_instruction` error responses, using the existing `session_header_name`/`session_id` pattern from the normal path.
- **Non-UTF-8 dump before rejection:** In `validate_upstream_body`, record the body as a base64 dump before returning `Err`, so operators can debug the upstream failure.

### Streaming Translation Bugs

- **Duplicate content_block_start:** Track the emitted content block index and skip `content_block_start` from `InteractionCreatedEvent` or `StepStart` when it would duplicate an already-active block at the same index.
- **Hardcoded ContentBlockStop:** Emit `ContentBlockStop` for the **last active** block index (tracked from `StepStart`), not hardcoded 0, on `InteractionCompletedEvent`.

### Ingress/Edge-Case Bugs

- **max_tokens truncation:** Use `u32::try_from` instead of `as u32`. On overflow, log a warning and clamp to `u32::MAX` (the practical limit for the Gemini API), rather than silently wrapping.
- **Health check query parameters:** Preserve the query string from the configured `endpoint_interactions` URL when building the health check request.
- **extend_lifetime end-of-message:** Change `after_prefix.find(|c: char| !c.is_ascii_digit())` to handle `None` by treating the entire remaining string as the timestamp.
- **Non-split ingress_response_dump:** Record the translated ingress response body as an `ingress_response_dump` on the non-split success path, matching the split-path behavior.

## Scope

### In Scope

- All 15 bug fixes listed above
- Unit tests for each fix (following the red-green TDD pattern)
- Updates to affected specs in `openspec/specs/`

### Out of Scope

- Architectural refactoring beyond what each fix requires
- New features or protocol support
- Changes to non-interactions handlers (except `validate_upstream_body`, `router.rs` health check, `extend_lifetime`)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions_handler.rs` | Yes | 11 fixes across split-send, streaming, error paths, diagnostics |
| `session.rs` | Yes | Startup recovery wiring, periodic eviction |
| `lib.rs` | Yes | `validate_upstream_body` dump-before-reject |
| `router.rs` | Yes | Health check query parameter preservation |
| `control.rs` | Yes | `extend_lifetime` end-of-message fix |
| Config | No | No config changes |
| API | No | No API surface changes (bug fixes only) |

## Architecture Considerations

All fixes follow existing patterns:
- Typed struct construction (no raw `Value` threading)
- `RequestDiagnostics` guard for stats-dump parity
- `SessionStore` atomic update pattern
- Existing helper functions (`session_header_name`, `ok_with_session_header`, etc.)

No new architectural patterns are introduced.

## Success Criteria

- [ ] System-instruction-only split requests return valid model responses (not empty fallback)
- [ ] Split-send retry after chunk failure does not duplicate upstream content
- [ ] Chunk serialized size never exceeds `proxy_limit` for subsequent chunks with `previous_interaction_id`
- [ ] Streaming crash recovery: pending sessions are verified on restart
- [ ] Expired sessions are evicted during normal operation (not just at startup)
- [ ] `cancel_interaction`/`delete_interaction` HTTP failures are detected and surfaced
- [ ] Split-path error responses include session/request headers
- [ ] Non-UTF-8 upstream bodies appear in diagnostics dumps (base64-encoded)
- [ ] No duplicate `content_block_start` events in streaming translation
- [ ] `ContentBlockStop` uses the correct block index on stream completion
- [ ] `max_tokens` values above `u32::MAX` are clamped (not wrapped)
- [ ] Health checks preserve query parameters from configured interactions endpoints
- [ ] `extend_lifetime` works when timestamp is at the end of the message
- [ ] Non-split interactions success path records `ingress_response_dump`

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Split-send atomic-update changes retry behavior | Low | Medium | Tests cover retry scenarios; partial-success chain has different IDs but no content duplication |
| Pending recovery adds startup latency | Low | Low | Recovery calls are parallelized; only affects sessions that were pending at shutdown |
| Streaming block-index tracking changes client-visible events | Low | Medium | Track active block index from StepStart events; validated against Gemini API behavior |

---

## Archive Information

**Archived:** 2026-06-24
**Duration:** 1 day
**Outcome:** Successfully implemented — all 15 bugs fixed

### Files Modified
- `src/interactions_handler.rs` — 11 fixes: clamp_max_tokens, split-send atomic session updates, last_active_index streaming tracking, ingress_response_dump, split error session headers, system-instruction response storage, chunk size estimation, streaming pending=true, cancel/delete HTTP status checks, non-UTF-8 dump recording, duplicate content_block_start removal
- `src/session.rs` — expired session eviction on get_or_create
- `src/lib.rs` — validate_upstream_body returns dump on error, pending session recovery at startup
- `src/router.rs` — health check preserves query parameters for interactions endpoints
- `src/control.rs` — extend_lifetime handles timestamp at end of message
- `src/anthropic.rs` — non-UTF-8 dump recording on validate failure
- `src/openai.rs` — non-UTF-8 dump recording on validate failure

### Specs Updated
- `openspec/specs/protocol-conversion.md` — split-send invariants, streaming translation fixes, max_tokens clamping, extend_lifetime EOM
- `openspec/specs/routing.md` — health check query params, session persistence recovery/eviction, streaming pending semantics
- `openspec/specs/diagnostics.md` — ingress response dump documentation

### Quality Gate
- `cargo test --locked` — 366 tests pass
- `cargo fmt --check` — clean
- `cargo clippy --locked -- -D warnings` — clean
