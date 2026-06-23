# Proposal: Strip Anthropic Fields from OpenAI Egress

**Change ID:** `fix-anthropic-openai-translation-leaks`
**Created:** 2026-06-23
**Status:** Implemented

---

## Problem Statement

When Anthropic-format ingress (e.g., from Claude CLI) is translated to OpenAI egress via `anyllm_translate` 0.9, two classes of fields leak into the outgoing request body, causing the OpenAI API to reject the request with 400:

1. **Anthropic-specific `extra` fields leak through `req.extra.clone()`.** `anyllm_translate`'s `MessageCreateRequest` uses `#[serde(flatten)] pub extra` to capture unknown Anthropic fields like `context_management` and `output_config`. The translation function `anthropic_to_openai_request` copies this map verbatim into `ChatCompletionRequest.extra` (`message_map/request.rs:141`), which is then flattened into the top-level JSON. OpenAI rejects unknown parameters — confirmed by direct curl: `"Unknown parameter: 'context_management'."`.

2. **`max_tokens` is set alongside `max_completion_tokens` for models that reject it.** Both fields are set from `req.max_tokens` during translation. Newer OpenAI models (gpt-5.5, gpt-5.3-codex, o-series) reject the legacy `max_tokens` field and require only `max_completion_tokens`. Confirmed by direct curl: `"Unsupported parameter: 'max_tokens' is not supported with this model."`.

The `anyllm_translate` crate already strips `max_tokens` for o-series models (`is_o_series_model`), but `gpt-5.*` models don't match that pattern.

## Proposed Solution

Add a `sanitize_openai_egress` helper in `src/openai.rs` that cleans the translated `ChatCompletionRequest` before serialization:

1. Remove `context_management` and `output_config` from `extra`
2. Set `max_tokens = None` when `max_completion_tokens` is present (always true for Anthropic→OpenAI translation)

Call the helper after `cap_openai_max_tokens` in both `handle_sync_manual` and `handle_stream_manual`.

When `route.max_tokens` is set but `route.max_completion_tokens` is not, ensure the limit transfers to `max_completion_tokens` before `max_tokens` is removed — so route-level token caps are not lost.

## Scope

### In Scope
- `ChatCompletionRequest.extra` cleanup (`context_management`, `output_config`)
- `max_tokens` removal when `max_completion_tokens` is present
- Route-level `max_tokens` limit preservation
- Both streaming and non-streaming translation paths

### Out of Scope
- Changes to `anyllm_translate` itself (dependency)
- Passthrough paths (OpenAI→OpenAI, Anthropic→Anthropic) — not affected
- Anthropic→Interactions translation — already handles extra fields via `build_interactions_request`

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `src/openai.rs` | Yes | New `sanitize_openai_egress` function + two call sites + 6 unit tests |
| `src/relay.rs` | No | Unchanged |
| Config | No | No new config options |
| Tests | Yes | 6 unit tests for `sanitize_openai_egress` |
| Docs | No | Internal implementation detail |

## Success Criteria

- [x] `gpt-5.5` model accepts requests via inf-splitter (no 400 errors)
- [x] `cargo test --locked` passes (266 tests, including 6 new `sanitize_openai_egress` tests)
- [x] `cargo clippy --locked -- -D warnings` clean
- [x] `cargo fmt --check` clean
- [x] Existing models (non-gpt-5.*) continue to work
- [ ] Verified end-to-end with Claude CLI `ping` through inf-splitter

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Older OpenAI models require `max_tokens` and reject `max_completion_tokens` | Very Low | Med | `max_completion_tokens` is the documented modern replacement; all current OpenAI models support it |
| New Anthropic `extra` fields leak in the future | Low | Low | Only stripping known fields — new fields will still leak unless added to the removal list. Mitigated by explicit test failures if this regresses |
| `route.max_tokens` limit lost when `max_tokens` removed | Low | Med | Limit is transferred to `max_completion_tokens` before removal |

---

## Archive Information

**Archived:** 2026-06-23 20:54
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/openai.rs` — `sanitize_openai_egress` helper + 2 call sites + 6 unit tests

### Specs Updated
- `openspec/specs/protocol-conversion.md` — added "Anthropic extra Field Sanitization" requirement, updated "Anthropic→OpenAI Translation" and "Token Limit Injection"
