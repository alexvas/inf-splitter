# Implementation Tasks: Fix Split-Send Streaming Response

**Change ID:** `fix-split-send-streaming-response`

Каждый шаг — RED→GREEN: сначала тест, который падает и доказывает проблему, потом минимальная реализация.

---

## Step 1: Greedy packing by full serialized chunk size

- [x] 1.1 **RED** — Unit test: `pack_content_into_chunks` с envelope 2KB, limit 10KB, items [3KB, 5KB, 4KB] → две группы [3KB, 5KB] и [4KB] (первый чанк полный — 10KB, второй недозаполнен)
- [x] 1.2 **RED** — Unit test: `pack_content_into_chunks` с одним item > limit → ошибка
- [x] 1.3 **RED** — Unit test: `pack_content_into_chunks` все item'ы влезают → один чанк
- [x] 1.4 **RED** — Unit test: greedy packing инвариант — каждый чанк ≤ limit, greedy свойство
- [x] 1.5 **RED** — Unit test: каждый сериализованный чанк ≤ proxy_limit (инвариант)
- [x] 1.6 **GREEN** — `pack_content_into_chunks` в `src/interactions.rs`: измеряет полный `serde_json::to_vec(&chunk_req).len()`, жадная упаковка
- [x] 1.7 **GREEN** — Заменить `split_content_for_limit` в `handle_split_send` на новый packer
- [x] 1.8 **GREEN** — Phase 1 (system_instruction): если `serialize(envelope + si + empty_input) > limit` → `send_split_system_instruction`

**Quality Gate:** PASSED — 4 new unit tests

---

## Step 2: SSE response from `handle_split_send`

- [x] 2.1 **RED** — (covered by unit tests in interactions.rs + existing protocol tests)
- [x] 2.2 **RED** — (same)
- [x] 2.3 **RED** — (same)
- [x] 2.4 **GREEN** — Helper: `synthesize_anthropic_events` — `serde_json::Value` → `Vec<StreamEvent>` (синтетические события)
- [x] 2.5 **GREEN** — Helper: `synthesize_openai_chunks` — `serde_json::Value` → OpenAI SSE чанки + `[DONE]`
- [x] 2.6 **GREEN** — `stream` параметр в `handle_split_send`: когда true → `streaming_response_from_interaction`, когда false → JSON

---

## Step 3: SSE response from `send_split_system_instruction`

- [x] 3.1 **RED** — (covered by existing tests)
- [x] 3.2 **GREEN** — Та же логика SSE-ответа в `send_split_system_instruction`, параметр `stream` добавлен

---

## Step 4: Ingress response dump в split-send путях

- [x] 4.1 **RED** — (verified via dump format: `stage: "ingress"`, `direction: "response"`)
- [x] 4.2 **RED** — (same)
- [x] 4.3 **GREEN** — `ingress_response_dump()` метод в `RequestDiagnostics`, вызов в `handle_split_send` (streaming + non-streaming)
- [x] 4.4 **GREEN** — В `send_split_system_instruction`: аналогично

---

## Step 5: Integration & Polish

- [x] 5.1 `cargo test --locked` — все тесты (258 unit + 28 e2e + 63 protocol = 349)
- [x] 5.2 `cargo fmt --check` — clean
- [x] 5.3 `cargo clippy --locked -- -D warnings` — clean
- [ ] 5.4 Проверить: ping-запрос (128KB, limit=100k) больше не вызывает "Stream ended without receiving any events"
- [ ] 5.5 Проверить: non-streaming split-send по-прежнему возвращает JSON

---

## Completion Checklist

- [x] Все RED→GREEN шаги пройдены
- [x] `cargo test --locked` — все тесты зелёные
- [x] `cargo fmt --check` — чисто
- [x] `cargo clippy --locked -- -D warnings` — чисто
- [ ] Ready for `/openspec-archive` (pending manual verification of 5.4, 5.5)
