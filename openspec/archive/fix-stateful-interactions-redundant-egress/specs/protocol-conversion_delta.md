# Delta: Protocol Conversion

**Change ID:** `fix-stateful-interactions-redundant-egress`
**Affects:** `src/session.rs` (`compute_delta`), `src/interactions.rs` (`build_request_body`)

---

## MODIFIED

### Requirement: Anthropic → Interactions Translation

`InteractionsHandler` converts Anthropic ingress to `CreateModelInteractionParams`:

- Ingress body parsed at boundary — `model`, `stream`, `temperature`, `max_tokens` extracted as typed scalars
- `messages[]` → interactions `Content[]` via typed extractors
- `system` → `system_instruction` — **only on the first interaction** (when `previous_interaction_id` is `None`)
- `max_tokens` → `generation_config.max_output_tokens`
- `tools` and `tool_choice` extracted from ingress body — **only on the first interaction** (when `previous_interaction_id` is `None`)
- `previous_interaction_id` set from session state (if exists)
- All parameters passed as typed scalars to `build_interactions_request_anthopic`, which returns `CreateModelInteractionParams` directly

Only messages not yet delivered to the session are included (delta computation). Control messages are stripped before construction.

#### Scenario: First request in session
- GIVEN no prior session state
- WHEN Anthropic request with 3 messages arrives
- THEN all 3 messages are translated, no `previous_interaction_id` sent
- AND `system_instruction`, `tools`, and `tool_choice` are included in the outgoing request

#### Scenario: Subsequent request — delta + chain
- GIVEN session has `{interaction_id: "abc123", delivered_count: 3}`
- WHEN Anthropic request with 5 messages arrives (same session)
- THEN only messages [3..5] are sent, `previous_interaction_id: "abc123"` is set
- AND `system_instruction`, `tools`, and `tool_choice` are **absent** (interaction reuses existing config)

#### Scenario: Subsequent request — no new messages
- GIVEN session has `{interaction_id: "abc123", delivered_count: 3}`
- WHEN Anthropic request with 3 messages arrives (same 3 messages, no new content)
- THEN `compute_delta(3, 3)` returns `(3, 3)` — an empty slice, no messages sent
- AND `system_instruction`, `tools`, and `tool_choice` are **absent**

#### Scenario: Context reset — fewer messages than delivered
- GIVEN session has `{interaction_id: "abc123", delivered_count: 5}`
- WHEN Anthropic request with 2 messages arrives (client started new conversation)
- THEN `compute_delta(5, 2)` returns `(0, 2)` — re-send all 2 messages
- AND `system_instruction`, `tools`, and `tool_choice` are included (new interaction, `previous_interaction_id` is `None`)

#### Scenario: System instruction split — chunks correctly chained
- GIVEN a large `system_instruction` that exceeds `proxy_limit`
- WHEN `send_split_system_instruction` splits the text and sends chunks
- THEN chunk 1 has `system_instruction = part[0]`, no `previous_interaction_id`
- AND chunk N has `system_instruction = part[N]`, `previous_interaction_id = chunk_N-1.id`
- AND the split path uses `build_chunk_request` (not `build_request_body`), so the `is_first` guard does not apply

---

### Requirement: OpenAI → Interactions Translation

OpenAI ingress → `CreateModelInteractionParams`:
- Ingress body parsed at boundary — `model`, `stream`, `temperature`, `max_tokens` extracted as typed scalars
- `messages[]` → interactions `Content[]` via typed extractors
- System message (role=system) → `system_instruction` — **only on the first interaction**
- `max_tokens` → `generation_config.max_output_tokens`
- `tools` and `tool_choice` extracted from ingress body — **only on the first interaction**

#### Scenario: First request — tools and system_instruction present
- GIVEN no prior session state
- WHEN OpenAI request with `tools` and a system message arrives
- THEN `system_instruction`, `tools`, and `tool_choice` are included in the outgoing `CreateModelInteractionParams`

#### Scenario: Subsequent request — tools and system_instruction absent
- GIVEN session has `{interaction_id: "abc123", delivered_count: 2}`
- WHEN OpenAI request with same tools and system message arrives (same session)
- THEN only new messages are sent (delta)
- AND `system_instruction`, `tools`, and `tool_choice` are **absent** from the outgoing request
