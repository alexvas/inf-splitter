# Delta: Diagnostics

**Change ID:** `fix-interactions-protocol-bugs`
**Affects:** `src/interactions_handler.rs`

---

## MODIFIED

### Requirement: RequestDiagnostics Session Guard (v2)

**Change:** In the streaming path (`handle_stream_response`), all early-return paths inside the spawned tokio task must call `guard.finish()` before returning. Previously, the guard was simply dropped, triggering the safety-net log.

#### Scenario: Client disconnect during streaming
- GIVEN an interactions stream is in progress
- WHEN the client disconnects (causing `tx.send()` to fail)
- THEN `guard.finish()` is called with the accumulated stats before `return`
- AND no `diagnostics guard dropped without finish` error is logged

#### Scenario: Stream chunk error
- GIVEN an interactions stream is in progress
- WHEN the upstream stream returns an error chunk
- THEN `guard.finish()` is called before the error is forwarded and `return`
- AND no `diagnostics guard dropped without finish` error is logged

#### Scenario: Normal stream completion unchanged
- GIVEN an interactions stream completes normally
- WHEN the stream ends (all chunks received, `interaction.completed` processed)
- THEN `guard.finish()` is called exactly once (existing behavior, unchanged)
