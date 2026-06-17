# Implementation Tasks: Startup Config Validation

**Change ID:** `config-validation`

Each step is a RED→GREEN pair: test first, then implement.

---

## Step 1: Empty/whitespace model name in list

- [x] 1.1 **RED** — Unit test: `models = ["valid", ""]` → error; `models = ["valid", "  "]` → error; `models = ["valid-model"]` → ok
- [x] 1.2 **GREEN** — Validate each entry in `parse_models` List branch: trim, reject if empty. Error message names the section.

---

## Step 2: Per-model drop_fields unknown model

- [x] 2.1 **RED** — Unit test: `[s.drop_fields] "unknown" = ["field"]` when `models = ["known"]` → error naming both section and unknown model
- [x] 2.2 **GREEN** — In `from_file_config`, after building `model_names`, validate `drop_fields` PerModel keys against it. New `ConfigError::UnknownDropModel { section, model }` variant.

---

## Step 3: Unknown fields in [defaults]

- [x] 3.1 **RED** — Unit test: `[defaults]` with `max_tokens = 4096` ✓; `[defaults]` with `endpoint_openai = "http://x"` → TOML parse error
- [x] 3.2 **GREEN** — Add `#[serde(deny_unknown_fields)]` to `DefaultConfig`.

---

## Step 4: Affected tests fixup

- [x] 4.1 **GREEN** — Fix any existing tests/configs that relied on the old lax behavior (if any). None found — all pre-existing tests pass unchanged.

---

## Quality Gate

- [x] `cargo test --locked` — all 144 tests pass
- [x] `cargo fmt --check` — clean
- [x] `cargo clippy --locked -- -D warnings` — clean

---

## Completion Checklist

- [x] All red→green steps pass
- [x] All quality gates passed
- [x] Ready for `/openspec-archive`
