# Delta: Request Header Forwarding

**Change ID:** `fix-header-correlation-mapping`
**Affects:** `src/auth.rs`, `src/openai.rs`, `src/anthropic.rs`

---

## ADDED

### Requirement: Protocol-Aware Correlation Header Mapping

`forward_request_headers_map` receives a `protocol: Protocol` parameter and applies protocol-specific correlation header mapping. `x-request-id` is **never** forwarded as-is — it is mapped to the upstream's correlation header.

**Anthropic upstream** (`Protocol::Anthropic`):

| Client header | Action |
|--------------|--------|
| `x-request-id` | Map to `x-claude-code-session-id`, do NOT forward as-is |
| `X-Client-Request-Id` | Map to `x-claude-code-session-id`, do NOT forward as-is |
| `x-claude-code-session-id` | Passthrough |

**OpenAI upstream** (`Protocol::OpenAi`):

| Client header | Action |
|--------------|--------|
| `x-request-id` | Map to `X-Client-Request-Id`, do NOT forward as-is |
| `x-claude-code-session-id` | Map to `X-Client-Request-Id`, do NOT forward as-is |
| `X-Client-Request-Id` | Passthrough |

#### Scenario: Anthropic upstream gets x-claude-code-session-id from X-Client-Request-Id
- GIVEN client sends `X-Client-Request-Id: client-1` (no `x-claude-code-session-id`)
- WHEN `forward_request_headers_map` is called with `Protocol::Anthropic`
- THEN `x-claude-code-session-id: client-1` is set
- AND `X-Client-Request-Id` is NOT present in upstream headers

#### Scenario: OpenAI upstream gets X-Client-Request-Id from x-claude-code-session-id
- GIVEN client sends `x-claude-code-session-id: sess-1` (no `X-Client-Request-Id`)
- WHEN `forward_request_headers_map` is called with `Protocol::OpenAi`
- THEN `X-Client-Request-Id: sess-1` is set
- AND `x-claude-code-session-id` is NOT present in upstream headers

#### Scenario: x-request-id never forwarded, always mapped
- GIVEN client sends `x-request-id: req-abc`
- WHEN forwarding to Anthropic upstream
- THEN upstream receives `x-claude-code-session-id: req-abc`, no `x-request-id`
- WHEN forwarding to OpenAI upstream
- THEN upstream receives `X-Client-Request-Id: req-abc`, no `x-request-id`

#### Scenario: Client's own correlation header preserved when it matches protocol
- GIVEN client sends `x-claude-code-session-id: sess-1`
- WHEN forwarding to Anthropic upstream
- THEN `x-claude-code-session-id: sess-1` is forwarded as-is (passthrough, no duplicate)

---

## MODIFIED

### Requirement: forward_request_headers_map Signature

**Change:** Added `protocol: Protocol` parameter.

```rust
// Before
pub fn forward_request_headers_map(api_key: Option<&str>, headers: &HeaderMap) -> HeaderMap

// After
pub fn forward_request_headers_map(api_key: Option<&str>, headers: &HeaderMap, protocol: Protocol) -> HeaderMap
```

`forward_request_headers` wrapper also gains the parameter.

---

## REMOVED

- Reciprocal blind mapping (`x-request-id ↔ x-claude-code-session-id` always added) — replaced by protocol-aware mapping
- `x-request-id` as-is forwarding to upstream — always mapped to protocol-specific header instead
