# Proposal: Replace body_too_large_hint_statuses with error_translation

**Change ID:** `replace-body-too-large-hint-with-error-translation`
**Created:** 2026-06-17
**Status:** Archived

---

## Problem Statement

`body_too_large_hint_statuses` is a narrow, single-purpose setting: it controls which HTTP status codes get a hardcoded hint appended to the error message. This is both too rigid (cannot customize the hint text per status code) and too narrow (only appends text, cannot replace the entire body).

When upstreams return cryptic or misleading error messages for well-understood failures (e.g., 413 payload too large, 502 bad gateway), the operator should be able to translate those upstream error bodies into clean, client-facing messages — either by matching a substring in the upstream body or by matching the status code alone.

## Proposed Solution

Remove `body_too_large_hint_statuses` and replace it with an optional `[[error_translation]]` array of tables in the TOML config. Each rule maps an upstream error response to a replacement body:

```toml
[[error_translation]]
status = 413
ingress = "some vague message substring"
egress = "body too large"

[[error_translation]]
status = 502
egress = "BODY TOO LARGE"
```

**Rules of operation:**
1. If `ingress` is set: match when upstream response status == `status` AND body contains the `ingress` substring → replace entire body with `egress`
2. If `ingress` is absent/empty: match when upstream response status == `status` → replace entire body with `egress`
3. Rules are evaluated in order, first match wins
4. If no rule matches, the upstream error body is passed through unchanged
5. An empty `error_translation` list (or omitting it entirely) disables all translation — errors pass through as-is

**Scope:** error translation only applies to upstream error responses (relayed through handlers). The local 413 from tower-http's `RequestBodyLimitLayer` is handled separately via the existing middleware, which is simplified: it no longer appends the hardcoded hint, just wraps the error in JSON.

## Scope

### In Scope
- Remove `body_too_large_hint_statuses` from `Config`, `FileConfig`, all references
- Remove `append_size_hint()` and `BODY_TOO_LARGE_HINT` constant from `lib.rs`
- Remove `hint_statuses` field from `OpenAiHandler` and `AnthropicHandler`
- Add `ErrorTranslationRule` struct and `error_translation: Vec<ErrorTranslationRule>` to `Config`
- Add `apply_error_translation()` function in `lib.rs`
- Update all 8 upstream error paths (4 in openai.rs, 4 in anthropic.rs) to call `apply_error_translation` instead of `append_size_hint`
- Simplify 413 middleware in `build_app()` — keep JSON error but drop the hardcoded hint
- Update config spec
- Update routing spec
- Update all three READMEs
- Update example config

### Out of Scope
- Translating successful (2xx) response bodies
- Regex-based ingress matching (substring only)
- Translation of streaming error responses (SSE)
- Per-section error translation overrides (global only)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `config.rs` | Yes | Remove `body_too_large_hint_statuses`, add `ErrorTranslationRule`, parse `[[error_translation]]` |
| `lib.rs` | Yes | Remove `append_size_hint` + `BODY_TOO_LARGE_HINT`, add `apply_error_translation`, simplify 413 middleware |
| `openai.rs` | Yes | Remove `hint_statuses` field, update 4 error paths |
| `anthropic.rs` | Yes | Remove `hint_statuses` field, update 4 error paths |
| Config spec | Yes | Remove requirement for `body_too_large_hint_statuses`, add `error_translation` |
| Routing spec | Yes | Remove Body Too Large Hint requirement, add Error Translation requirement |
| READMEs (×3) | Yes | Update global settings table |
| Example config | Yes | Add commented example |
| Tests | Yes | Update existing tests, add new translation tests |

## Architecture Considerations

Follows the existing pattern from `drop_fields`: config is parsed into a simple `Vec` of rules, stored in `Arc<Config>`, passed to handler constructors.

The `apply_error_translation()` function sits in `lib.rs` alongside the other shared utilities (`apply_egress_transforms`, `drop_fields_from_value`, etc.). It takes status code, body string, and the rules slice — returns the (possibly translated) body string.

The local 413 middleware no longer has access to the error translation rules (by design — it handles tower-http layer errors, not upstream errors). It simply produces a clean JSON error: `{"type":"error","error":{"type":"invalid_request_error","message":"Request body exceeds limit."}}` without the hint suffix.

## Success Criteria

- [ ] `body_too_large_hint_statuses` is completely removed from code, config, specs, and READMEs
- [ ] `[[error_translation]]` is parsed correctly from TOML
- [ ] All 8 upstream error paths apply translation rules
- [ ] Empty/absent `error_translation` results in no-op (pass-through)
- [ ] Existing tests pass
- [ ] New tests cover: rule matching (status only, status+substring), no-match passthrough, first-match ordering
- [ ] `cargo fmt --check`, `cargo clippy --locked -- -D warnings`, `cargo test --locked` all pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Breaking config change | Low | Low | `body_too_large_hint_statuses` was optional with default `[413]`; operators who customized it explicitly can express the same logic via `[[error_translation]]` rules. |
| Substring false positives | Low | Low | `ingress` matching is substring-based by design; operators control the specificity of their match strings |

---

## Archive Information

**Archived:** 2026-06-17
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/config.rs` — Added `ErrorTranslationRule`, removed `body_too_large_hint_statuses`
- `src/lib.rs` — Added `apply_error_translation()`, removed `append_size_hint()`/`BODY_TOO_LARGE_HINT`
- `src/openai.rs` — Replaced `hint_statuses` with `error_translation` in all error paths
- `src/anthropic.rs` — Replaced `hint_statuses` with `error_translation` in all error paths
- `config/inf-splitter.toml.example` — Added commented `[[error_translation]]` examples
- `README.md`, `README.en.md`, `README.zh.md` — Updated global settings tables
- `CLAUDE.md` — Updated config model summary

### Specs Updated
- `openspec/specs/config.md` — Removed `body_too_large_hint_statuses`, added Error Translation Rules
- `openspec/specs/routing.md` — Removed Body Too Large Hint, added Upstream Error Body Translation
