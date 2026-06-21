# Delta: Diagnostics — Every Handler Records Dump Events

**Change ID:** `add-interactions-dump-recording`
**Affects:** `src/interactions_handler.rs`, diagnostics dump output

---

## ADDED

### Requirement: Every protocol handler records dump events

Every protocol handler (OpenAI passthrough, Anthropic passthrough, protocol conversion, interactions) must record the same categories of dump events for every request:

- **ingress request** — the original client body as received by the proxy
- **egress request** — the body actually sent upstream (after token caps, protocol translation, control message stripping, etc.)
- **egress response** — the raw upstream response body (up to 1 MiB for streaming)

All dump events for a single request share the same `request_id` as the corresponding stats event.

The Anthropic and OpenAI handlers already satisfied this invariant. This change brings the interactions handler (`InteractionsHandler`) into compliance.

#### Scenario: Non-streaming interactions request produces dump
- GIVEN `dump_mode = "all"` and `dump_output` is set to a file path
- AND a non-streaming request arrives at the interactions handler
- WHEN the request completes with status 200
- THEN three dump lines are written: ingress request, egress request, egress response
- AND all three lines share the same `request_id`

#### Scenario: Streaming interactions request produces dump
- GIVEN `dump_mode = "all"` and `dump_output` is set to a file path
- AND a streaming request arrives at the interactions handler
- WHEN the SSE stream completes with status 200
- THEN ingress and egress request dump lines are written
- AND a response dump line is written with the raw SSE body (up to 1 MiB)

#### Scenario: Error response produces dump
- GIVEN `dump_mode = "all"` (or `"error"`) and `dump_output` is set to a file path
- AND the interactions upstream returns a non-2xx status
- WHEN the error is handled
- THEN ingress and egress request dump lines are written
- AND a response dump line is written containing the error body

#### Scenario: No dump when dump_mode is off
- GIVEN `dump_mode = "off"`
- WHEN a request completes (any handler)
- THEN no dump lines are written for that request

#### Scenario: Non-UTF8 body in dump
- GIVEN the request or response body is not valid UTF-8
- WHEN a dump event is recorded
- THEN `body` is base64-encoded with `"encoding": "base64"`
- AND a `tracing::warn!` is emitted

#### Scenario: Shared request_id between dump and stats
- GIVEN `dump_mode = "all"` and `stats_mode = "all"`
- AND a request completes via the interactions handler
- WHEN both dump and stats events are recorded
- THEN all dump lines and the stats line for that request share the same `request_id`
