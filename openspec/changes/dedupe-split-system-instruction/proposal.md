## Why

Gemini split system-instruction sending has duplicate streaming and non-streaming planning logic. Duplication makes future fixes risky and keeps session-update behavior harder to verify.

## What Changes

- Extract shared split-piece planning into one `Vec<SplitPiecePlan>` flow.
- Reuse one loop for streaming and non-streaming split pieces.
- Keep response reader selection stream-specific while sharing request body construction, send, ack, and session-update steps.
- Preserve external API behavior and upstream request semantics.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

None.

## Impact

- Affected code: Gemini/interactions split system-instruction request handling around streaming and non-streaming paths.
- Affected APIs: none.
- Dependencies: none.
- Tests: existing split system-instruction coverage plus targeted refactor tests if needed.
