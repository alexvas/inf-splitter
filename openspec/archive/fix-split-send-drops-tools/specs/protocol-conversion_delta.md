# Delta: Protocol Conversion

**Change ID:** `fix-split-send-drops-tools`
**Affects:** `src/interactions_handler.rs` (`handle_split_send`, `send_split_system_instruction`)

---

## MODIFIED

### Requirement: Proxy-Limit Split-Send

When a request exceeds `proxy_limit`, content is split into chunks and sent sequentially. Each chunk is a separate `CreateModelInteractionParams` request chained via `previous_interaction_id`.

**First-chunk-only fields** (`tools`, `generation_config`, `system_instruction`) are set on the first chunk (when `current_prev` is `None`, meaning the chunk creates a new interaction). Subsequent chunks omit these fields — they reuse the interaction's existing configuration.

The `send_split_system_instruction` path (when system_instruction itself needs splitting) follows the same rule: `tools` and `generation_config` are attached to the first system-instruction chunk only.

#### Scenario: First chunk carries tools and generation_config
- GIVEN a request with `tools` and `generation_config` that exceeds `proxy_limit`
- WHEN `handle_split_send` builds the first chunk (`current_prev` is `None`)
- THEN the chunk request includes `tools` and `generation_config`
- AND `system_instruction` is included on the first chunk

#### Scenario: Subsequent chunks omit tools and generation_config
- GIVEN a split-send sequence where the first chunk already created the interaction
- WHEN `handle_split_send` builds chunk 2+ (`current_prev` is `Some(...)`)
- THEN `tools` and `generation_config` are `None`
- AND `system_instruction` is `None`

#### Scenario: System instruction split first chunk carries tools
- GIVEN a request where both content and system_instruction need splitting
- WHEN `send_split_system_instruction` builds the first system-instruction chunk
- THEN the chunk includes `tools` and `generation_config`
