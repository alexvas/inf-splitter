## Context

`src/interactions_handler.rs` has two split system-instruction paths: streaming at `send_split_system_instruction_streaming` and non-streaming inside `send_split_system_instruction`. Both paths split the system instruction, create an in-flight batch, iterate system-instruction pieces, optionally attach the first content chunk, build chunk request bodies, send each chunk, acknowledge pieces, and update session chaining. The primary difference is response reading: streaming parses SSE for `interaction.created`, while non-streaming reads JSON `Interaction` responses.

## Goals / Non-Goals

**Goals:**

- Represent the full split-send sequence as one ordered `Vec<SplitPiecePlan>`.
- Share request construction and send/ack/session progression for streaming and non-streaming paths.
- Keep response extraction stream-specific through a small reader abstraction or helper branch.
- Preserve existing split sizing, tools/generation config placement, drop-fields behavior, diagnostics dumps, in-flight batch semantics, and final response shape.

**Non-Goals:**

- Change public HTTP API behavior.
- Change split sizing algorithms or content packing.
- Change interactions API payload schema.
- Rewrite unrelated split-send or protocol conversion paths.

## Decisions

- Introduce `SplitPiecePlan` near split-send helpers with fields `input`, `system_instruction`, and `include_tools_config`.
  - Rationale: these are the per-piece choices duplicated between stream and non-stream paths.
  - Alternative considered: pass closures for every per-piece variant. Rejected because it keeps planning implicit and harder to test.

- Build plans once after `split_text_for_limit` and before batch creation.
  - Rationale: `plans.len()` becomes the authoritative in-flight piece count, replacing duplicate `sys_parts.len() + extra_chunks` calculations.
  - Alternative considered: keep separate system/content loops and only share body serialization. Rejected because it leaves most duplication intact.

- Use one loop to convert each `SplitPiecePlan` into `CreateModelInteractionParams`, serialize to JSON, apply `drop_fields`, dump diagnostics, mark started, send chunk, read interaction id, and ack.
  - Rationale: preserves ordering and makes retry/session semantics identical for streaming and non-streaming.
  - Alternative considered: leave streaming and non-streaming loops separate. Rejected because bug fixes would still need two edits.

- Isolate response reading behind helpers selected by `stream`.
  - Rationale: streaming needs SSE buffering and `interaction.created` parsing; non-streaming needs body validation, diagnostics response dump, and JSON deserialization. Keeping this separate avoids mixing protocol-specific logic into planning.
  - Alternative considered: normalize upstream responses into a common enum with all fields. Acceptable if implementation stays small, but helper methods are simpler.

## Risks / Trade-offs

- Missed subtle streaming/non-streaming behavior difference → Mitigation: compare existing tests and add focused assertions for first piece tools/config, previous interaction chaining, and final session state in both modes.
- Larger helper signatures due to existing diagnostics/session parameters → Mitigation: keep refactor local and avoid broad type restructuring unless needed.
- Response-reader abstraction can hide diagnostics differences → Mitigation: keep stream and non-stream reader helpers explicit and protocol-specific.
