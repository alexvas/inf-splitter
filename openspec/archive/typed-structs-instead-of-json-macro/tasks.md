# Implementation Tasks: Replace manual json!() with typed structs

**Change ID:** `typed-structs-instead-of-json-macro`

---

## Phase 1: Foundation (build.rs)

- [ ] 1.1 Add `Default` to struct derive list
- [ ] 1.2 Add `Default` to tagged enum derive list
- [ ] 1.3 Add `Default` to untagged enum derive list
- [ ] 1.4 `cargo check` — verify generation compiles

---

## Phase 2: Core refactoring (interactions.rs)

- [ ] 2.1 Update imports
- [ ] 2.2 Rewrite `build_request_body()` to construct `CreateModelInteractionParams`
- [ ] 2.3 Add `build_chunk_request()` helper
- [ ] 2.4 `cargo test -p inf-splitter` — verify unit tests

---

## Phase 3: Handler refactoring (interactions_handler.rs)

- [ ] 3.1 Update imports
- [ ] 3.2 Replace system instruction size check `json!()`
- [ ] 3.3 Replace `handle_split_send` chunk `json!()`
- [ ] 3.4 Replace `send_split_system_instruction` chunk `json!()` (x2)

---

## Phase 4: Documentation

- [ ] 4.1 Strengthen CLAUDE.md serialization rule
- [ ] 4.2 Full verification: `cargo fmt --check && cargo clippy --locked -- -D warnings && cargo test --locked`

---

## Completion Checklist

- [ ] All phases complete
- [ ] All quality gates passed
- [ ] Ready for `/openspec-archive`
