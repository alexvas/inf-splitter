# Delta: Diagnostics

**Change ID:** `fix-interactions-dump-and-split-bugs`
**Affects:** `src/diagnostics.rs`, `src/auth.rs`, `src/interactions_handler.rs`, `src/openai.rs`, `src/anthropic.rs`

---

## MODIFIED

### Requirement: Dump Event Format — JSON body embedding

The `body` field of a `DumpEvent` is serialized as an embedded JSON value when
the body bytes parse as valid JSON. Otherwise, it remains a string.

#### Scenario: Valid JSON body
- GIVEN a dump body containing `{"error":{"message":"permission denied"}}`
- WHEN the DumpEvent is serialized
- THEN the body is embedded as `"body":{"error":{"message":"permission denied"}}`

#### Scenario: Non-JSON body
- GIVEN a dump body containing `plain text error`
- WHEN the DumpEvent is serialized
- THEN the body is serialized as `"body":"plain text error"`

#### Scenario: Empty body
- GIVEN a dump body containing `""`
- WHEN the DumpEvent is serialized
- THEN the body is serialized as `"body":""` (empty JSON string parsing fails)

#### Scenario: Base64 body
- GIVEN a dump body with binary content
- WHEN the DumpEvent is serialized
- THEN the body remains a base64-encoded string with `"encoding":"base64"`

### Requirement: Sensitive Header Masking (Diagnostics level)

Header values for `x-goog-api-key`, `authorization`, and `x-api-key` are
masked as `"***"` in all dump output, regardless of entry point. The masking is
case-insensitive and applied at the lowest level:
- `Diagnostics::record_request_dump` — via `header_pairs_with_masking`
- `Diagnostics::record_response_dump` — via `mask_header_values`
- `RequestDiagnostics::ingress_dump` / `egress_dump` — via `header_pairs_with_masking` (headers stored pending, flushed directly to `record_dump`)

This covers: Router direct calls (non-UTF8 body, parse errors), Relay/DiagnosticStream (streaming response dumps), and all three handlers via RequestDiagnostics.

#### Scenario: Egress dump with api_key
- GIVEN `x-goog-api-key: AIzaSy...` is set in egress headers
- WHEN the dump is recorded through ANY path (Diagnostics, RequestDiagnostics, DiagnosticStream)
- THEN the header appears as `["x-goog-api-key", "***"]`

#### Scenario: Router early-error dump
- GIVEN a non-UTF8 ingress body triggers `state.diagnostics.record_request_dump(...)` with empty headers
- WHEN the dump is recorded
- THEN the dump succeeds with `headers: []` (unchanged, no sensitive data)

#### Scenario: DiagnosticStream streaming response dump
- GIVEN a streaming response from upstream with auth headers
- WHEN `DiagnosticStream` calls `diagnostics.record_response_dump(...)`
- THEN any `x-goog-api-key` / `authorization` / `x-api-key` values in headers are masked

#### Scenario: Non-sensitive headers pass through
- GIVEN `x-request-id: trace-12345` and `content-type: application/json`
- WHEN the dump is recorded
- THEN these headers appear with their original values unchanged

---

## ADDED

### Requirement: Egress dump uses actual upstream headers (ALL handlers)

All `egress_dump` calls in `interactions_handler.rs`, `openai.rs`, and
`anthropic.rs` receive the actual headers sent to the upstream (after
`build_interactions_headers` / `forward_request_headers` transformation),
not the ingress `request_headers`.

#### Scenario: Interactions handler with API key
- GIVEN `api_key = "some-key"` in config
- WHEN an interactions request is sent
- THEN the egress dump shows `x-goog-api-key: ***` (masked) and does NOT show client `authorization`
- AND `Api-Revision` and `Content-Type` headers are present

#### Scenario: OpenAI/Anthropic handler with API key
- GIVEN `api_key = "some-key"` in config
- WHEN a passthrough/conversion request is sent
- THEN the egress dump shows `x-api-key: ***` and `authorization: ***` (masked, from `forward_request_headers`)
- AND does NOT show client auth headers

#### Scenario: No API key configured
- GIVEN no `api_key` in config
- WHEN a request is sent through any handler
- THEN the egress dump shows the forwarded client `authorization` (masked)
- AND `x-goog-api-key` / `x-api-key` are NOT present

### Requirement: proxy_limit size check uses full request body

The proxy_limit size check measures the full serialized
`CreateModelInteractionParams` body, including `system_instruction`, `tools`,
and all other fields — not just the `input` ContentList.

#### Scenario: Small input but large system_instruction
- GIVEN `proxy_limit = "100k"` and a request with 10K input but 120K system_instruction
- WHEN the request is processed
- THEN the full body exceeds 100K limit and splitting is triggered

#### Scenario: Small request under limit
- GIVEN `proxy_limit = "100k"` and a request with 50K total body
- WHEN the request is processed
- THEN no splitting is triggered
