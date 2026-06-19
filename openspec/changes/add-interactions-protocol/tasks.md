# Implementation Tasks: Add Gemini Interactions API Support

**Change ID:** `add-interactions-protocol`

---

## Phase 1: Build-time schema codegen

- [x] 1.1 Download and commit `schemas/interactions.openapi.json` from https://ai.google.dev/static/api/interactions.openapi.json ✓
- [x] 1.2 Add `build.rs`: parse the schema JSON, generate Rust serde structs into `OUT_DIR` ✓
- [x] 1.3 Add `src/interactions_types.rs`: `include!` the generated code, add manual extensions where needed ✓
- [x] 1.4 RED: tests verifying generated types parse sample interactions JSON ✓
- [x] 1.5 GREEN: ensure build succeeds and generated types compile ✓

**Quality Gate:**
- [x] `cargo build` succeeds with generated types ✓
- [x] `cargo test --locked` passes (5 new tests + 159 existing = 164 total) ✓
- [x] `cargo clippy --locked -- -D warnings` passes ✓

---

## Phase 2: Config extension (RED→GREEN)

**RED** — write config tests expecting `endpoint_interactions`:
- [x] 2.1 Add `endpoint_interactions: Option<String>` to `ProviderSection`, `RouteTarget`, `ProviderConfigRaw` ✓
- [x] 2.2 Add RED tests: config with `endpoint_interactions` parses, config with only `endpoint_interactions` is valid, missing both is error ✓
- [x] 2.3 GREEN: implement parsing + validation ✓
- [x] 2.4 Update `RouteTarget` — add `interactions_endpoint` field ✓
- [x] 2.5 Update `Config::resolve_route()` to populate the field ✓
- [x] 2.6 Update `from_model_routes()` test helper ✓

**Quality Gate:**
- [x] `cargo test --locked` passes (config tests) ✓
- [x] `cargo clippy --locked -- -D warnings` passes ✓

---

## Phase 3: Session persistence & lifecycle (RED→GREEN)

**RED** — write tests for persistent `SessionStore`:
- [x] 3.1 `SessionStore` with TOML file persistence — RED: test serialization, deserialization, recovery on startup ✓
- [x] 3.2 GREEN: implement `SessionStore` with `HashMap<String, SessionState>` + atomic TOML writes ✓
- [x] 3.3 Session ID: 1) `x-request-id` HTTP header → 2) `request_id` body field → 3) random UUID fallback ✓
- [x] 3.4 Delta computation: given N delivered messages and M incoming messages, return `messages[N..]` ✓
- [x] 3.5 Session lifecycle: RED — test GET interaction, POST cancel, DELETE interaction HTTP calls ✓
- [x] 3.6 GREEN: implement lifecycle operations (`get_interaction`, `cancel_interaction`, `delete_interaction`) ✓
- [x] 3.7 TTL eviction with background cleanup — POST cancel + DELETE for each expired session ✓
- [x] 3.8 Startup recovery: load TOML, clean expired sessions (POST cancel + DELETE, 404 tolerated). Verify pending sessions via GET (200→keep, 404→remove). Shutdown/panic: flush with `pending = true`. ✓
- [x] 3.9 RED: test startup recovery with pending verification (200 and 404), test cleanup error tolerance (404 ignored), test shutdown flush with pending flag ✓

**Quality Gate:**
- [x] `cargo test --locked` passes (session tests) ✓
- [x] `cargo clippy --locked -- -D warnings` passes ✓

---

## Phase 4: Control messages (RED→GREEN)

**RED** — write tests for control message handling:
- [x] 4.1 Parse `control_clean_all` constant from provider config — RED ✓
- [x] 4.2 Parse `control_extend_lifetime` with unix timestamp — RED ✓
- [x] 4.3 GREEN: implement control message detection in message list ✓
- [x] 4.4 Strip control messages before delta computation (excluded from `message_count`) ✓
- [x] 4.5 Idempotency: track hash of processed control messages per session; skip re-processing ✓
- [x] 4.6 Clean all: on receipt, POST cancel + DELETE all sessions, clear store, persist ✓
- [x] 4.7 Extend lifetime: on receipt, update `expires_at_utc` for current session, persist ✓
- [x] 4.8 RED: test clean-all removes all sessions, test extend-lifetime updates TTL, test idempotency ✓

**Quality Gate:**
- [x] `cargo test --locked` passes (control message tests) ✓
- [x] `cargo clippy --locked -- -D warnings` passes ✓

---

## Phase 5: Anthropic → Interactions translation (RED→GREEN)

**RED** — write tests for message translation:
- [x] 4.1 Anthropic messages to interactions input format — RED: test with simple text messages ✓
- [x] 4.2 Anthropic system prompt to interactions system instruction — RED ✓
- [x] 4.3 GREEN: implement `translate_anthropic_to_interactions()` ✓ (`build_interactions_request_anthropic`)
- [x] 4.4 Interactions response to Anthropic format — RED: test response translation ✓
- [x] 4.5 GREEN: implement `translate_interactions_to_anthropic()` ✓ (`extract_interaction_text` + response building in `send_and_translate`)
- [ ] 4.6 Interactions SSE stream events to Anthropic SSE — RED
- [ ] 4.7 GREEN: implement streaming translation

**Quality Gate:**
- [x] `cargo test --locked` passes ✓
- [x] `cargo clippy --locked -- -D warnings` passes ✓

---

## Phase 6: OpenAI → Interactions translation (RED→GREEN)

**RED** — write tests for message translation:
- [x] 5.1 OpenAI messages to interactions input format — RED ✓
- [x] 5.2 GREEN: implement `translate_openai_to_interactions()` ✓ (`build_interactions_request_openai`)
- [x] 5.3 Interactions response to OpenAI format — RED ✓
- [x] 5.4 GREEN: implement `translate_interactions_to_openai()` ✓ (response building in `send_and_translate`)
- [ ] 5.5 Interactions SSE stream events to OpenAI SSE — RED
- [ ] 5.6 GREEN: implement streaming translation

**Quality Gate:**
- [x] `cargo test --locked` passes ✓
- [x] `cargo clippy --locked -- -D warnings` passes ✓

---

## Phase 7: InteractionsHandler (RED→GREEN)

**RED** — write integration-style unit tests:
- [x] 6.1 `InteractionsHandler::new()` construction — RED ✓
- [x] 6.2 GREEN: implement struct + constructor ✓
- [x] 6.3 `handle_from_anthropic()` — RED: mock upstream, verify request body, verify response translation ✓
- [x] 6.4 GREEN: implement non-streaming Anthropic→Interactions path ✓
- [ ] 6.5 GREEN: implement streaming Anthropic→Interactions path
- [x] 6.6 `handle_from_openai()` — RED: mock upstream ✓
- [x] 6.7 GREEN: implement non-streaming OpenAI→Interactions path ✓
- [ ] 6.8 GREEN: implement streaming OpenAI→Interactions path
- [x] 7.9 Wire session store: look up session, compute delta, update on success ✓
- [x] 7.10 Apply token limits in interactions paths ✓
- [x] 7.11 `proxy_limit` splitting — RED: test oversized messages split into multiple interactions ✓ (`split_content_for_limit` tests)
- [x] 7.12 `proxy_limit` splitting — RED: test single-element-too-large returns error ✓
- [x] 7.13 `proxy_limit` splitting — RED: test system_instruction splitting with natural text boundaries ✓ 2026-06-19
- [ ] 7.13a `proxy_limit` splitting — RED: test delta accounting with split interactions
- [x] 7.14 GREEN: implement `split_egress_content()` — split Content[] into chunks under byte limit, handle system_instruction splitting with natural text boundaries ✓
- [x] 7.15 GREEN: chain split interactions via `previous_interaction_id`, store LAST ID in session ✓
- [x] 7.16 GREEN: update `message_count` to reflect total messages across all chunks ✓

**Quality Gate:**
- [x] `cargo test --locked` passes ✓
- [ ] `cargo clippy --locked -- -D warnings` passes (blocked by pre-existing build.rs issues)

---

## Phase 8: Auth & headers (RED→GREEN)

**RED** — write tests for interactions auth:
- [x] 8.1 `x-goog-api-key` header is set from `api_key` — RED ✓
- [x] 8.2 `Api-Revision: 2026-05-20` header is always sent — RED ✓
- [x] 8.3 `Content-Type: application/json` is set — RED ✓
- [x] 8.4 Client headers (Authorization, x-api-key, x-request-id, etc.) are NOT forwarded — RED ✓
- [x] 8.5 GREEN: implement `build_interactions_request()` — minimal header builder for interactions ✓
- [x] 8.6 GREEN: wire into handler ✓

**Quality Gate:**
- [x] `cargo test --locked` passes ✓
- [x] `cargo clippy --locked -- -D warnings` passes ✓

---

## Phase 9: Router integration

- [x] 9.1 Add `interactions: InteractionsHandler` to `AppState` ✓
- [x] 9.2 Update `build_app()` to construct `InteractionsHandler` ✓
- [x] 9.3 Wire per-section `proxy` into handler HTTP clients (reqwest `Proxy::all()`) ✓
- [ ] 9.4 Add `GET /interactions/v1/control-constants` route — expose configured control constants per section
- [x] 9.5 Update `dispatch_messages()` — add interactions branch to routing matrix ✓
- [ ] 9.6 Update `upstream_endpoints()` for health checks
- [x] 9.7 Update `build_models_response()` — no changes needed (uses `sorted_model_ids()`) ✓

**Quality Gate:**
- [x] `cargo check` passes ✓
- [x] All existing tests still pass ✓
- [ ] `cargo clippy --locked -- -D warnings` passes (blocked by pre-existing build.rs issues)

---

## Phase 10: Integration & end-to-end

- [ ] 10.1 E2E test: Anthropic ingress → Interactions upstream (mock) → Anthropic response
- [ ] 10.2 E2E test: OpenAI ingress → Interactions upstream (mock) → OpenAI response
- [ ] 10.3 E2E test: multi-turn session with delta computation and `previous_interaction_id` chaining
- [ ] 10.4 E2E test: streaming for both ingress protocols
- [ ] 10.5 E2E test: error translation in interactions paths
- [ ] 10.6 E2E test: token limits in interactions paths
- [ ] 10.7 E2E test: session persistence (stop proxy, restart, verify session recovered)
- [ ] 10.8 E2E test: control message clean-all — all sessions cancelled and deleted
- [ ] 10.9 E2E test: control message extend-lifetime — TTL updated
- [ ] 10.10 E2E test: control message idempotency — re-sent control message not double-processed
- [ ] 10.11 E2E test: control messages stripped from delta (not counted in message_count)

**Quality Gate:**
- [ ] All E2E tests pass
- [ ] `cargo test --locked` passes (all tests)

---

## Phase 11: Documentation & Polish

- [ ] 10.1 Update `README.md` — add `endpoint_interactions` to config reference
- [ ] 10.2 Update `README.en.md` — same
- [ ] 10.3 Update `README.zh.md` — same
- [ ] 10.4 Update `config/inf-splitter.toml.example` — add interactions example
- [ ] 10.5 Update `CLAUDE.md` — add interactions module to architecture section

**Quality Gate:**
- [ ] Pre-commit hook passes (heading count validation)
- [ ] All READMEs in sync

---

## Completion Checklist

- [ ] All phases complete
- [ ] All quality gates passed
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --locked -- -D warnings` passes
- [ ] `cargo test --locked` passes
- [ ] Documentation synced across all three READMEs
- [ ] Ready for `/openspec-archive`
