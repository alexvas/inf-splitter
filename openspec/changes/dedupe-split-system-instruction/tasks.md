## 1. RED — Tests First

- [ ] 1.1 Add failing unit tests for `SplitPiecePlan` ordering: system-instruction fragments first, final system piece carries first content chunk, remaining content chunks follow.
- [ ] 1.2 Add failing unit test that only first planned piece includes tools/generation config.
- [ ] 1.3 Add or update failing regression coverage proving streaming and non-streaming split system-instruction paths use same planned piece count and piece indexes.

## 2. GREEN — Production Code

- [ ] 2.1 Add `SplitPiecePlan` type near interactions split-send helpers with `input`, `system_instruction`, and `include_tools_config` fields.
- [ ] 2.2 Add helper that converts `sys_parts` plus packed content chunks into ordered `Vec<SplitPiecePlan>`.
- [ ] 2.3 Refactor split system-instruction handling to create in-flight batch from `plans.len()`.
- [ ] 2.4 Replace duplicate streaming/non-streaming piece loops with one loop that builds chunk requests from `SplitPiecePlan`, applies `drop_fields`, dumps egress, marks started, sends, reads interaction id, acknowledges, and advances `current_prev`.
- [ ] 2.5 Keep stream-specific and non-stream-specific response readers explicit while preserving SSE buffering, response dumps, validation, interaction parsing, and error handling.
- [ ] 2.6 Preserve final response construction and session updates for both streaming and non-streaming callers.

## 3. INTROSPECT — Review Behavior

- [ ] 3.1 Compare old and new flow for tools/generation config placement, previous interaction chaining, diagnostics dumps, in-flight batch writes, and final session updates.
- [ ] 3.2 Identify any helper signatures or control-flow branches that became harder to reason about.

## 4. REFINE — Clean Up

- [ ] 4.1 Remove dead duplicate split system-instruction loop code.
- [ ] 4.2 Simplify helper names, comments, and error paths without changing behavior.
- [ ] 4.3 Run `cargo fmt` after code cleanup.

## 5. VERIFY — Checks

- [ ] 5.1 Run targeted split system-instruction tests for streaming and non-streaming paths.
- [ ] 5.2 Run `cargo fmt --check`.
- [ ] 5.3 Run `cargo clippy --locked`.
- [ ] 5.4 Run `cargo test --locked`.
