# Delta: Diagnostics

**Change ID:** `guard-error-path-invariant`
**Affects:** `src/diagnostics.rs`, `src/interactions_handler.rs`

---

## ADDED

### Requirement: No Unfinalized Guard on Error Return

В любой функции, владеющей `RequestDiagnostics` (guard), каждый `?`-проброс ошибки ДО вызова `guard.finish()` / `guard.finish_with_error()` является нарушением инварианта. `.map_err()?` — частный случай, не менее опасный.

**Правило:** перед `return Err(...)` guard должен быть финализирован через `guard.finish_with_error(status, ..., err_msg)`.

**Проверка при code review:** если в функции есть `guard: RequestDiagnostics` и встречается `?` до строки с `guard.finish(...)` — это красный флаг.

#### Scenario: send_and_translate network send failure

- GIVEN `send_and_translate` отправляет запрос в upstream
- AND `upstream.send().await` возвращает `Err(reqwest::Error)`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(502, ..., error_message)` вызывается ДО `return Err(...)`

#### Scenario: send_and_translate response read failure

- GIVEN `send_and_translate` читает тело ответа
- AND `upstream.bytes().await` возвращает `Err(reqwest::Error)`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(502, ...)` вызывается ДО `return Err(...)`

#### Scenario: send_and_translate body validation failure

- GIVEN `send_and_translate` валидирует тело ответа
- AND `validate_upstream_body()` возвращает `Err(AppError)`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(502, ...)` вызывается ДО `return Err(...)`

#### Scenario: send_and_translate interaction parse failure

- GIVEN `send_and_translate` парсит JSON ответа как `Interaction`
- AND `serde_json::from_str()` возвращает `Err`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(502, ...)` вызывается ДО `return Err(...)`
- AND `response_body.len()` передаётся как `response_size` для диагностики

#### Scenario: send_and_translate response build failure

- GIVEN `send_and_translate` собирает ingress-ответ через `build_response_from_interaction`
- AND функция возвращает `Err(String)`
- WHEN ошибка обрабатывается
- THEN `guard.finish_with_error(500, ...)` вызывается ДО `return Err(AppError::Internal(...))`

#### Scenario: handle_split_send chunk packing failure

- GIVEN `handle_split_send` пакует контент в чанки через `pack_content_into_chunks`
- AND single content item превышает `proxy_limit`
- WHEN `pack_content_into_chunks` возвращает `Err("content item too large for proxy_limit: ...")`
- THEN `guard.finish_with_error(400, ...)` вызывается ДО `return Err(AppError::BadRequest(...))`

#### Scenario: send_split_system_instruction split failure

- GIVEN `send_split_system_instruction` разбивает system_instruction через `split_text_for_limit`
- AND текст не удаётся разбить под лимит
- WHEN `split_text_for_limit` возвращает `Err`
- THEN `guard.finish_with_error(400, ...)` вызывается ДО `return Err(AppError::BadRequest(...))`

---

## MODIFIED

### Requirement: Every Protocol Handler Records Dump and Stats Events

Расширен перечень покрытых сценариев:

#### Scenario: Interactions proxy_limit chunk packing fails (was: split check fails)

- GIVEN an Anthropic or OpenAI ingress request routed to the interactions handler
- AND the request size exceeds `proxy_limit`
- AND EITHER `can_split_under_limit` determines the request cannot be split
- OR `pack_content_into_chunks` fails because a single content item exceeds the limit
- OR `split_text_for_limit` fails because system_instruction cannot be split
- WHEN the handler returns a 400 error
- THEN an ingress dump is written to `dump_output`
- AND a stats entry is written to `stats_output` with `status: 400` and the full error message in the `error` field
- AND guard IS finalized via `finish_with_error` (not dropped)

#### Scenario: send_and_translate error paths finalized

- GIVEN `send_and_translate` encounters any error after sending the request upstream
- AND the error occurs on the non-streaming path
- WHEN the error propagates
- THEN `guard.finish_with_error()` is called BEFORE the error return
- AND the error path covers: network read failure, body validation failure, JSON parse failure, response build failure

---

## REMOVED

(None)
