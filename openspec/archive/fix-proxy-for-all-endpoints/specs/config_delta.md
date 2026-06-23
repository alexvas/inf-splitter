# Delta: Configuration

**Change ID:** `fix-proxy-for-all-endpoints`
**Affects:** `openspec/specs/config.md` — "Per-Endpoint Proxy" requirement

---

## MODIFIED

### Requirement: Per-Endpoint Proxy

Provider sections can specify an explicit proxy for outgoing requests:

```toml
proxy = "http://127.0.0.1:8081"
# or
proxy = "socks5://172.17.0.1:3823"
```

If set, all outgoing upstream requests for that section go through the configured proxy, regardless of which endpoint type is used (`endpoint_openai`, `endpoint_anthropic`, or `endpoint_interactions`). If absent, reqwest falls back to environment proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`).

#### Scenario: Proxy with OpenAI endpoint
- GIVEN `proxy = "http://127.0.0.1:8081"` and `endpoint_openai = "https://api.openai.com"` in provider section
- WHEN an outgoing request is sent for that section
- THEN reqwest routes through `http://127.0.0.1:8081`
- AND the upstream receives the request from the proxy, not from inf-splitter directly

#### Scenario: Proxy with Anthropic endpoint
- GIVEN `proxy = "socks5://172.17.0.1:3823"` and `endpoint_anthropic = "https://api.anthropic.com"` in provider section
- WHEN an outgoing request is sent for that section
- THEN reqwest routes through `socks5://172.17.0.1:3823`

#### Scenario: Proxy with Interactions endpoint (existing, unchanged)
- GIVEN `proxy = "http://127.0.0.1:8081"` and `endpoint_interactions = "https://generativelanguage.googleapis.com/v1beta/interactions"`
- WHEN outgoing requests are sent for that section
- THEN reqwest routes through `http://127.0.0.1:8081`

#### Scenario: No proxy configured
- GIVEN no `proxy` in provider section
- WHEN outgoing requests are sent
- THEN reqwest uses environment proxy variables (if any)

#### Scenario: Different proxies per section
- GIVEN section A has `proxy = "http://proxy-a:8080"` and section B has `proxy = "http://proxy-b:8081"`
- WHEN requests are routed to section A and section B
- THEN section A requests go through `proxy-a` and section B requests go through `proxy-b`
