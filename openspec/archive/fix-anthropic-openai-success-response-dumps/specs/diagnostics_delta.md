# Delta: Diagnostics

**Change ID:** `fix-anthropic-openai-success-response-dumps`
**Affects:** `src/openai.rs`, `src/anthropic.rs`, dump event coverage tests

---

## ADDED

### Requirement: Protocol Conversion Success Paths Record Upstream Response Dumps

Successful protocol-conversion requests must record an `egress` `response` dump containing the raw upstream response body and upstream response headers before translating that response back to the ingress protocol.

#### Scenario: Non-streaming Anthropic→OpenAI success produces response dump
- GIVEN a route has only `endpoint_openai` configured
- AND an Anthropic `/v1/messages` request is routed to that section
- AND diagnostics `dump_mode = "all"`
- WHEN the OpenAI upstream returns a successful JSON chat completion response
- THEN the dump output contains three entries for the same `request_id`: `ingress/request`, `egress/request`, and `egress/response`
- AND the `egress/response` body is the raw OpenAI upstream response body
- AND the `egress/response` headers contain the upstream response headers

#### Scenario: Non-streaming OpenAI→Anthropic success produces response dump
- GIVEN a route has only `endpoint_anthropic` configured
- AND an OpenAI `/v1/chat/completions` request is routed to that section
- AND diagnostics `dump_mode = "all"`
- WHEN the Anthropic upstream returns a successful JSON message response
- THEN the dump output contains three entries for the same `request_id`: `ingress/request`, `egress/request`, and `egress/response`
- AND the `egress/response` body is the raw Anthropic upstream response body
- AND the `egress/response` headers contain the upstream response headers

#### Scenario: Streaming Anthropic→OpenAI success produces response dump
- GIVEN a route has only `endpoint_openai` configured
- AND an Anthropic streaming `/v1/messages` request is routed to that section
- AND diagnostics `dump_mode = "all"`
- WHEN the OpenAI upstream SSE stream completes successfully
- THEN the dump output contains an `egress/response` entry for the same `request_id` as the request dumps and stats event
- AND the response dump body contains the captured raw OpenAI SSE response up to the configured streaming dump limit
- AND the response dump headers contain the upstream response headers

#### Scenario: Streaming OpenAI→Anthropic success produces response dump
- GIVEN a route has only `endpoint_anthropic` configured
- AND an OpenAI streaming `/v1/chat/completions` request is routed to that section
- AND diagnostics `dump_mode = "all"`
- WHEN the Anthropic upstream SSE stream completes successfully
- THEN the dump output contains an `egress/response` entry for the same `request_id` as the request dumps and stats event
- AND the response dump body contains the captured raw Anthropic SSE response up to the configured streaming dump limit
- AND the response dump headers contain the upstream response headers

---

## MODIFIED

### Requirement: Every Protocol Handler Records Dump Events

Every protocol handler, including all successful protocol-conversion paths, must record the same categories of dump events for every completed request.

#### Scenario: Protocol conversion success produces full dump set
- GIVEN `dump_mode = "all"`
- AND a request completes successfully through any protocol conversion direction
- WHEN dump events are flushed
- THEN the output includes `ingress/request`, `egress/request`, and `egress/response` entries
- AND all entries share the same `request_id`

### Requirement: Every Protocol Handler Records Stats Events

Streaming protocol-conversion handlers must record stats after the stream completes, using the same `request_id` as the request and response dumps.

#### Scenario: Streaming conversion stats are finalized on stream completion
- GIVEN `stats_mode = "all"`
- AND a streaming protocol-conversion request completes successfully
- WHEN the translated client stream reaches EOF
- THEN a stats line is written with `streaming: true`
- AND the stats line shares `request_id` with the response dump

### Requirement: Interactions Client Disconnect Response Dump Exception Is Documented

Interactions streaming handlers may finalize the guard with status 499 without recording an `egress/response` dump when the downstream client disconnects before the upstream stream completes. The code path must contain a short comment explaining that no response dump is recorded because there is no complete upstream response to dump.

#### Scenario: Interactions stream client disconnect is documented
- GIVEN an interactions stream is forwarding upstream events to the client
- WHEN sending to the client fails before upstream EOF
- THEN the guard is finalized with status 499
- AND the implementation comment explains why no response dump is recorded on that early-return path

---

## REMOVED

(None)
