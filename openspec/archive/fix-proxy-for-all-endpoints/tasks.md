# Implementation Tasks: Fix — Proxy for All Endpoints

**Change ID:** `fix-proxy-for-all-endpoints`

---

## Phase 1: OpenAiHandler proxy support

- [x] 1.1 Replace `http: HttpClient` with `clients: HashMap<String, HttpClient>` in `OpenAiHandler`
- [x] 1.2 Add `fn get_client(&self, proxy: Option<&str>) -> &HttpClient` helper that looks up pre-built client
- [x] 1.3 Update all `self.http.post(...)` call sites to use `self.get_client(route.proxy.as_deref())`
- [x] 1.4 Update `OpenAiHandler::new()` to pre-populate the map from all section proxies

**Quality Gate:** PASSED — cargo check, cargo test --locked, cargo clippy, cargo fmt --check

---

## Phase 2: AnthropicHandler proxy support

- [x] 2.1 Replace `client: Client` with `clients: HashMap<String, Client>` in `AnthropicHandler`
- [x] 2.2 Add `fn get_client(&self, proxy: Option<&str>) -> &Client` helper
- [x] 2.3 Update `build_upstream_request` to use `self.get_client(route.proxy.as_deref())`
- [x] 2.4 Update `AnthropicHandler::new()` to pre-populate the map from all section proxies

**Quality Gate:** PASSED — cargo check, cargo test --locked, cargo clippy, cargo fmt --check

---

## Phase 3: Spec update

- [x] 3.1 Update `openspec/specs/config.md` — rename "Per-Endpoint Proxy" → "Per-Section Proxy", clarify applies to all endpoint types
- [x] 3.2 Add scenario confirming proxy works with `endpoint_openai`
- [x] 3.3 Add scenario confirming proxy works with `endpoint_anthropic`

**Quality Gate:** PASSED

---

## Phase 4: Integration verification

- [x] 4.1 Run full test suite (`cargo test --locked`) — 258 unit + 28 integration + 63 e2e, all passed
- [x] 4.2 Run `cargo clippy --locked -- -D warnings` — clean
- [x] 4.3 Run `cargo fmt --check` — clean

**Quality Gate:** PASSED — all checks green
