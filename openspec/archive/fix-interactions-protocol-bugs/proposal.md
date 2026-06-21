# Proposal: Fix Interactions Protocol Bugs

**Change ID:** `fix-interactions-protocol-bugs`
**Created:** 2026-06-21
**Status:** Completed

---

## Problem Statement

Three bugs in the interactions protocol handler cause degraded behavior for Gemini upstream:

1. **SSE event deserialization fails for `interaction.created`**: The `Interaction` schema marks `created`, `updated`, `steps` as required. Gemini API sends `interaction.created` SSE events where the initial interaction object is incomplete (only `id`, `status`, `object`, `model` — no `created`/`steps`/`updated`). Deserialization of `InteractionSseEvent` fails with `missing field 'created'`, causing the entire SSE event to be silently dropped (`tracing::info!` log, `None` returned). This means the `message_start` + `content_block_start` events are never sent to the client for streaming requests.

2. **Diagnostics guard dropped without finish**: In `handle_stream_response`, the spawned tokio task has early-return paths (when `tx.send()` fails due to client disconnect, or when the chunk stream errors). These paths `return` without calling `guard.finish()`, triggering the `Drop` safety net that logs `tracing::error!("diagnostics guard dropped without finish")`.

3. **Tool definitions not forwarded to Interactions API**: `build_interactions_request_anthropic` and `build_interactions_request_openai` do not extract `tools` or `tool_choice` from ingress requests. Tool definitions from the client are silently dropped, so the Gemini model never sees them.

## Proposed Solution

### Fix 1: Patch schema in build.rs to make Interaction fields optional

Modify `build.rs` to patch the `Interaction` schema in-memory before code generation: remove `created`, `updated`, and `steps` from the `required` array. The Gemini API does not consistently include these in SSE event payloads. Code that accesses these fields must handle them as `Option<T>`.

### Fix 2: Call guard.finish() in all early-return paths in handle_stream_response

Add `guard.finish()` call before every `return` statement inside the spawned task:
- `tx.send(Ok(...)).await.is_err()` → call `guard.finish()` then `return`
- `tx.send(Err(...)).await` → call `guard.finish()` then `return`

### Fix 3: Forward tool definitions from ingress to interactions API

Extract `tools` and `tool_choice` from:
- Anthropic ingress body: `tools` array and `tool_choice` field
- OpenAI ingress body: `tools` array and `tool_choice` field

Pass them to `build_request_body()` and set on `CreateModelInteractionParams.tools` and `CreateModelInteractionParams.tool_config`.

## Scope

### In Scope
- Fix Interaction schema `required` array in build.rs
- Fix diagnostics guard early-return in `handle_stream_response`
- Forward `tools` and `tool_choice` from Anthropic/OpenAI ingress to interactions API
- Tests for all three fixes

### Out of Scope
- Supporting all tool definition formats (Anthropic `tool_choice` types beyond basic)
- Full support for tool use in non-streaming interactions response translation
- Changes to the control message system

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `build.rs` | Yes | Patch `Interaction.required` to remove `created`, `updated`, `steps` |
| `interactions.rs` | Yes | Extract `tools`/`tool_choice` from ingress, pass to `build_request_body` |
| `interactions_handler.rs` | Yes | Fix guard early-return; extract tools at ingress boundary |
| `interactions_types.rs` | No | Generated types change automatically via build.rs patch |

## Architecture Considerations

- The schema patching in `build.rs` is the minimal change — we avoid creating parallel Interaction types for SSE vs non-streaming contexts
- The guard fix is straightforward: ensure `finish()` is called before each `return` in the spawned task
- Tool forwarding follows the existing pattern of extracting typed fields at the ingress boundary and threading them through typed constructors

## Success Criteria

- [ ] `interaction.created` SSE events deserialize successfully (no `missing field 'created'` log entries)
- [ ] No `diagnostics guard dropped without finish` log entries for normal streaming interactions
- [ ] Tools from Anthropic ingress appear in the outgoing `CreateModelInteractionParams.tools`
- [ ] Tools from OpenAI ingress appear in the outgoing `CreateModelInteractionParams.tools`
- [ ] All existing tests pass
- [ ] `cargo fmt --check`, `cargo clippy`, `cargo test` all pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Making `steps` optional breaks non-streaming response code | Low | Medium | `extract_interaction_text` already handles missing steps via iterator chain; verify all access sites |
| Schema patch makes struct slightly mismatch OpenAPI spec | Low | Low | Fields are marked "output only" in spec anyway; API sends them differently in different contexts |
| Tool forwarding may expose API differences | Medium | Low | Basic line-level conversion; test with real Gemini models |

---

## Archive Information

**Archived:** 2026-06-21 12:30
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `build.rs` — Schema patching to make created/updated/steps optional, added `Function` to Default derives
- `src/interactions.rs` — Tool extraction helpers, tool forwarding through build_request_body, typed ToolChoice
- `src/interactions_handler.rs` — Wire up tool extraction in handlers, guard.finish() in streaming early-return paths
- `src/interactions_types.rs` — Manual `ToolChoice` enum, RED test for interaction.created deserialization

### Specs Updated
- `openspec/specs/protocol-conversion.md` — Schema patching, Anthropic→Interactions tools, OpenAI→Interactions tools, Function Default
- `openspec/specs/diagnostics.md` — Streaming client disconnect and stream error guard scenarios
