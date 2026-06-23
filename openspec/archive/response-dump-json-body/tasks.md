# Implementation Tasks: Response Dump JSON Body

**Change ID:** `response-dump-json-body`

---

## Phase 1: SSE Buffer Parsing

- [x] 1.1 Добавить хелпер `parse_sse_buffer_to_json_array(buf: &[u8]) -> DumpBody` в `src/sse.rs`
- [x] 1.2 Хелпер должен: разбить буфер на SSE-события по `\n\n`, извлечь `data:` строки, распарсить JSON, накопить в `Vec<Value>`, вернуть `DumpBody::Utf8(serialized_array)`
- [x] 1.3 При неудаче парсинга отдельного data-поля — пропускать (graceful), при полной неудаче — fallback на исходный текст
- [x] 1.4 Учесть обрезанный буфер (MAX_STREAMING_DUMP_BYTES): последнее incomplete событие отбросить

**Quality Gate:**
- [x] `cargo check` passes
- [x] `cargo clippy` passes

---

## Phase 2: Integrate in Handler

- [x] 2.1 В `handle_stream_response` заменить `dump_body_from_bytes(&dump_buffer)` на `parse_sse_buffer_to_json_array(&dump_buffer)`
- [x] 2.2 Убедиться, что `dump_body.is_base64()` проверка остаётся корректной
- [x] 2.3 Убрать предупреждение о non-utf8 (уже обрабатывается в хелпере)

**Quality Gate:**
- [x] `cargo check` passes
- [x] `cargo clippy` passes
- [x] `cargo test` passes

---

## Phase 3: Test

- [x] 3.1 Тест: SSE-буфер из двух событий → `parse_sse_buffer_to_json_array` возвращает JSON-массив из двух объектов
- [x] 3.2 Тест: обрезанный буфер → последнее incomplete событие отброшено
- [x] 3.3 Тест: не-JSON data строка → пропущена (skip [DONE])
- [x] 3.4 Тест: пустой буфер → `[]`
- [x] 3.5 Тест: non-UTF-8 → fallback на base64
- [x] 3.6 Тест: нет SSE-событий → fallback на исходный текст

**Quality Gate:**
- [x] Все тесты проходят (341 total, 6 новых)

---

## Completion Checklist

- [x] All phases complete
- [x] All quality gates passed
- [x] `cargo fmt --check` clean
- [x] `cargo clippy --locked -- -D warnings` clean
- [x] `cargo test --locked` all passing
