# Proposal: Deferred Response Dump Flush

**Change ID:** `deferred-response-dump-flush`
**Created:** 2026-06-23
**Status:** Archived
**Completed:** 2026-06-23

---

## Archive Information

**Archived:** 2026-06-23
**Duration:** <1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/diagnostics.rs` — `response_dump_pending`, deferred flush, `sync_data()` in `RotatingWriter::flush()`
- `tests/common/mod.rs` — `poll_diagnostics_file` stabilization

### Specs Updated
- `openspec/specs/diagnostics.md` — response dump deferral, RotatingWriter sync_data, poll stabilization

---

## Problem Statement

Тесты `passthrough_success_request_dumps_have_status` и подобные — флакуют. Причина: race condition между writer-потоком (пишет дампы в файл) и тестом (читает файл).

Три источника гонки:
1. `response_dump` отправляется в канал сразу, а `ingress_dump`/`egress_dump` — отложенно (flush в `finish`). Тест может прочитать файл между записью response-дампа и отложенных дампов.
2. `RotatingWriter::flush()` вызывает только `BufWriter::flush()` (сброс в буфер ОС), но не `fdatasync()` — данные могут остаться в page cache и не попасть на диск.
3. `poll_diagnostics_file` в тестах возвращает результат как только предикат satisfied — не ждёт стабилизации содержимого.

## Proposed Solution

1. Сделать `response_dump` и `response_dump_streaming` отложенными (как ingress/egress) — flush в `finish`/`finish_with_error` вместе со всеми.
2. Добавить `sync_data()` в `RotatingWriter::flush()` — гарантировать сброс с дискового кэша ОС.
3. Добавить стабилизацию в `poll_diagnostics_file` — после satisfaction подождать 20ms и перечитать; если размер не изменился — вернуть.

## Scope

### In Scope
- `RequestDiagnostics::response_dump` / `response_dump_streaming` — deferred
- `RotatingWriter::flush()` — `sync_data()`
- `poll_diagnostics_file` — стабилизация контента

### Out of Scope
- Изменение протокола канала (try_send → send)
- Гарантии ordering для нескольких одновременных запросов

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `diagnostics.rs` | Yes | `response_dump_pending`, `flush_deferred_dumps`, `sync_data` |
| `tests/common/mod.rs` | Yes | `poll_diagnostics_file` stabilization |

## Architecture Considerations

- Отложенные response-дампы следуют тому же паттерну, что ingress/egress — `StoredDump` в `Mutex<Option<...>>`
- Drop safety net (`impl Drop`) уже вызывает `flush_deferred_dumps` — response-дамп не потеряется при падении
- `sync_data()` добавляет системный вызов `fdatasync` на каждый flush — потенциально замедляет запись, но только при `flush_period = None` (по умолчанию)

## Success Criteria

- [ ] `passthrough_success_request_dumps_have_status` проходит 10/10 раз
- [ ] Все 341 тестов проходят
- [ ] `cargo fmt --check` чист
- [ ] `cargo clippy` чист
