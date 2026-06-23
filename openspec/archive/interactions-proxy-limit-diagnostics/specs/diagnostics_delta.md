# Delta: Diagnostics

**Change ID:** `interactions-proxy-limit-diagnostics`
**Affects:** diagnostics recording in interactions handler, error messages from `can_split_under_limit`

---

## MODIFIED

### Requirement: Every Protocol Handler Records Dump Events

Interactions handler must record an ingress dump **before** any sub-function is called, so that early error paths (control actions, `can_split_under_limit` failures) are covered.

#### Scenario: proxy_limit split check fails
- GIVEN an Anthropic or OpenAI ingress request routed to the interactions handler
- AND the request size exceeds `proxy_limit`
- AND `can_split_under_limit` determines the request cannot be split
- WHEN the handler returns a 400 error
- THEN an ingress dump is written to `dump_output`
- AND a stats entry is written to `stats_output` with `status: 400` and the full error message in the `error` field
- AND the client receives `400 bad request: Request cannot be split under proxy limit (see diagnostics for details)`

#### Scenario: control action executed
- GIVEN an interactions request containing a control message (clean_all or extend_lifetime)
- WHEN the handler executes the control action successfully
- THEN a stats entry is written with `status: 200`, `upstream: "control-action"`, `direction` matching the action type

---

## ADDED

### Requirement: Envelope Size Breakdown in can_split_under_limit Errors

When the non-splittable envelope exceeds `proxy_limit`, the error message must list each contributing field with its byte count and human-readable size.

#### Scenario: tools dominate the envelope
- GIVEN a request with 105 tools totaling 160 KiB
- AND `proxy_limit` set to 100 KiB
- WHEN `can_split_under_limit` checks the envelope
- THEN the error message includes lines like:
  - `model: 19 B`
  - `stream: 4 B`
  - `tools: 160.0 KiB`

### Requirement: Per-Tool Size Breakdown in can_split_under_limit Errors

When `can_split_under_limit` returns an error, and tools are present, the error message must include a per-tool size breakdown showing name, total serialized size, description size, and parameters schema size for each `Tool::Function`.

#### Scenario: real-world tool list breakdown
- GIVEN a request with 105 tools from a Claude Code session
- AND `proxy_limit` set to 100 KiB
- WHEN `can_split_under_limit` returns an error
- THEN the error message contains a section `Per-tool size breakdown (sorted by size):`
- AND each `Tool::Function` line shows `{name}: {total} (description: {desc_size}, parameters: {params_size})`
- AND non-Function tools show `({type_name}): {total}`
- AND tools are sorted by total size in descending order (heaviest first)
- AND sizes use human-readable units (B, KiB, MiB)

### Requirement: Lazy Tool Breakdown Computation

The per-tool size breakdown must only be computed when a limit error actually occurs, not on every request.

#### Scenario: request under limit
- GIVEN a request whose envelope fits within `proxy_limit`
- WHEN `can_split_under_limit` is called
- THEN `tool_size_breakdown` is never invoked
- AND no per-tool serialization overhead is incurred

### Requirement: format_bytes Helper

A `format_bytes(bytes: usize) -> String` function must format byte counts in human-readable units:
- `< 1024`: `"{n} B"`
- `< 1024*1024`: `"{n/1024:.1} KiB"`
- `>= 1024*1024`: `"{n/(1024*1024):.1} MiB"`
