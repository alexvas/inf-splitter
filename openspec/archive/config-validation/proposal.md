# Proposal: Startup Config Validation

**Change ID:** `config-validation`
**Created:** 2026-06-17
**Status:** Implementation Complete
**Completed:** 2026-06-17
**Archived:** 2026-06-17

---

## Problem Statement

The proxy already validates several config invariants at startup (duplicate models, missing endpoints, etc.), but three gaps let invalid configs through silently:

1. **Empty/whitespace model names in lists** — `models = ["valid", ""]` or `models = ["valid", "  "]` passes validation and registers a route for `""` (empty string). A client request with `"model": ""` would match it, routing to the wrong upstream.
2. **Per-model `drop_fields` referencing unknown models** — `[s.drop_fields]` with `"model-x" = ["field"]` when `model-x` is not listed in the section's `models` — a typo that silently does nothing. The operator thinks fields are being dropped but they're not.
3. **Unknown fields in `[defaults]` silently ignored** — `DefaultConfig` lacks `#[serde(deny_unknown_fields)]`, so `endpoint_openai = "..."` in `[defaults]` is silently dropped instead of erroring.

## Proposed Solution

Add three validation checks during `Config::from_file_config`:

**#1 — Trim and validate each model name in the List variant** of `parse_models`. Reject empty or whitespace-only model names with a clear error message pointing to the section.

**#2 — After building a section's model set, validate its `drop_fields`** against that set. If `drop_fields` is `PerModel` with keys that don't match any model in the section (and aren't `"all"`), return a `ConfigError` with the section name and the unknown key.

**#3 — Add `#[serde(deny_unknown_fields)]` to `DefaultConfig`** so that typos in `[defaults]` are rejected at TOML parse time instead of silently ignored.

## Scope

### In Scope
- Empty/whitespace model name in list → rejected with section name in error
- Per-model `drop_fields` key not matching any section model → rejected with model name in error
- Unknown fields in `[defaults]` → rejected with field name in TOML parse error

### Out of Scope
- URL validation for endpoint fields (too complex, fails at runtime anyway)
- Zero/negative token limit validation (0 is valid `u32`, operators may want it)
- `DiagnosticsConfigRaw` unknown field rejection (same pattern as `[defaults]` but less critical)
- Reserved section name collision with `[diagnostics]` (same `deny_unknown_fields` gap but lower risk)

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| Config model | Yes | `DefaultConfig` gains `deny_unknown_fields` |
| Config validation | Yes | `parse_models` List variant validates each entry; `from_file_config` validates drop_fields against model set |
| Config tests | Yes | Red-green tests for all three validations |
| ConfigError | Yes | New variant `UnknownDropModel { section, model }` |

## Success Criteria

- [ ] `models = ["valid", ""]` → startup fails with error naming the section
- [ ] `models = ["valid", "  "]` → startup fails with error naming the section
- [ ] `[s.drop_fields]` with `"unknown-model"` not in `models` → startup fails with error naming both
- [ ] `[defaults]` with unknown key → TOML parse fails with field name
- [ ] Existing valid configs continue to work unchanged
- [ ] `cargo fmt --check`, `cargo clippy`, `cargo test` all pass

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| `deny_unknown_fields` breaks existing configs with commented-out fields in `[defaults]` | Low | Medium | Comments are stripped by TOML parser, not treated as fields. Only actual key=value pairs trigger errors. |
| Per-model drop_fields validation runs before all sections are parsed | Low | Low | The drop_fields are validated against the section's own models list, not cross-section. No ordering dependency. |
