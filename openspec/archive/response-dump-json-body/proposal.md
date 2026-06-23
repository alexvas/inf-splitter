# Proposal: Response Dump JSON Body

**Change ID:** `response-dump-json-body`
**Created:** 2026-06-23
**Status:** Archived
**Completed:** 2026-06-23

---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/sse.rs` — added `parse_sse_buffer_to_json_array` helper + 6 unit tests
- `src/interactions_handler.rs` — `handle_stream_response`: replaced `dump_body_from_bytes` with `parse_sse_buffer_to_json_array`

### Specs Updated
- `openspec/specs/diagnostics.md` — added Streaming Response Dump Body Format and parse_sse_buffer_to_json_array Helper requirements

---

## Problem Statement

В dump-файле `direction=request` body сохраняется как встроенный JSON-объект (реализовано в `DumpEvent::Serialize`). А `direction=response` — как строка, даже если тело ответа является валидным JSON.

Причина: для стриминговых ответов `response_dump_streaming` получает сырой SSE-буфер (`dump_buffer`), в котором накоплены строки вида `data: {...}\n\n`. Это невалидный JSON → `serde_json::from_str` не срабатывает → body пишется строкой.

Не-стриминговые ответы (success path через `validated.dump`) этой проблемой не затронуты — там тело уже валидный JSON и корректно встраивается.

## Proposed Solution

В `response_dump_streaming` (и связанном коде) парсить SSE-буфер: извлекать `data:`-строки, для каждой парсить JSON, накапливать в `Vec<serde_json::Value>`, сериализовать как JSON-массив. Нераспаршенные строки оставлять как есть (graceful degradation).

Не-стриминговые ответы — без изменений (уже работают корректно через `validated.dump`).

## Scope

### In Scope
- `response_dump_streaming` в `interactions_handler.rs` — парсинг SSE-буфера в JSON-массив
- `DumpBody::Utf8` уже обрабатывается правильно в `DumpEvent::Serialize`
- Тест, проверяющий что стриминговый response dump содержит JSON-массив, а не строку

### Out of Scope
- Не-стриминговые response dump (уже работают)
- Error-path response dump (может быть plain text — это ожидаемое поведение)
- Изменение формата `DumpEvent`

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `interactions_handler.rs` | Yes | `handle_stream_response`: парсить SSE-буфер перед `response_dump_streaming` |
| `diagnostics.rs` | No | `DumpEvent::Serialize` уже правильно обрабатывает JSON |
| `sse.rs` | Possibly | Может понадобиться хелпер для парсинга SSE-событий из буфера |
| Tests | Yes | Новый тест в `protocol_conversion.rs` |

## Architecture Considerations

- Следует существующему паттерну: `DumpEvent::Serialize` уже умеет встраивать JSON
- Изменение только на стороне формирования body перед передачей в `response_dump_streaming`
- Буфер может быть обрезан (`MAX_STREAMING_DUMP_BYTES` = 65536) — incomplete events нужно обрабатывать gracefully
- Сырой SSE-текст теряться не должен — если парсинг не удался, оставляем как строку (fallback)

## Success Criteria

- [ ] Стриминговый response dump содержит `body` как JSON-массив объектов (событий)
- [ ] Каждый элемент массива — распаршенный JSON из `data:` строки SSE-события
- [ ] Не-стриминговый response dump продолжает работать (без изменений)
- [ ] При ошибке парсинга SSE — fallback на строку (graceful degradation)
- [ ] Все существующие тесты проходят
