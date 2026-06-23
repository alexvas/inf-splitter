# Proposal: Diagnostics Guard Error-Path Invariant

**Change ID:** `guard-error-path-invariant`
**Created:** 2026-06-23
**Status:** Implementation Complete
**Completed:** 2026-06-23

---

## Problem Statement

`RequestDiagnostics` — это session guard, который должен быть финализирован вызовом `finish()` или `finish_with_error()` на каждом пути выполнения. Если guard дропается без финализации, срабатывает Drop safety net: пишется `tracing::error!("diagnostics guard dropped without finish")` и stats-событие с `error: "diagnostics guard dropped without finish"`.

В `interactions_handler.rs` обнаружены семь путей, где ошибка пробрасывается через `?` (включая `.map_err()?`), а guard остаётся нефинализированным:

| Функция | Строка | Что роняло |
|---------|--------|-----------|
| `handle_split_send` | 777 | `pack_content_into_chunks()` → `?` |
| `send_split_system_instruction` | 971 | `split_text_for_limit()` → `?` |
| `send_and_translate` | 409 | `upstream.send().await?` |
| `send_and_translate` | 453 | `upstream.bytes().await?` |
| `send_and_translate` | 454 | `validate_upstream_body()?` |
| `send_and_translate` | 457 | `serde_json::from_str().map_err()?` |
| `send_and_translate` | 470 | `build_response_from_interaction().map_err()?` |

Все семь уже исправлены — каждый `?` заменён на явный `match` с вызовом `guard.finish_with_error()` перед `return Err(...)`.

## Proposed Solution

Закрепить в спеке инвариант: **любой `?` (включая `.map_err()?`) в функции, где есть нефинализированный guard, требует явной обработки — guard должен быть финализирован до проброса ошибки.** Поощряемая альтернатива: явный `match` с вызовом `guard.finish_with_error()` перед `return Err(...)`.

Также добавить сценарии в спек diagnostics.md, покрывающие все семь исправленных путей.

## Scope

### In Scope
- Инвариант в спеке diagnostics.md
- Сценарии для семи исправленных путей
- Сценарий для `send_and_translate` error paths

### Out of Scope
- Механическая проверка инварианта (линтер / clippy lint)
- Изменения в других handler'ах (openai.rs, anthropic.rs — там guard уже корректно финализирован)
- Рефакторинг `send_and_translate` для уменьшения бойлерплейта

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| diagnostics.rs | No | Guard и так имеет Drop safety net |
| interactions_handler.rs | Already fixed | Все семь путей исправлены |
| diagnostics.md spec | Yes | Добавить инвариант и сценарии |

## Success Criteria

- [x] Все семь guard-drop путей в `interactions_handler.rs` исправлены
- [x] `cargo fmt --check`, `cargo clippy --locked -- -D warnings`, `cargo test --locked` — всё зелёное
- [x] Спек diagnostics.md содержит инвариант и покрывает все семь сценариев
- [x] `openspec-archive` применён

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Новый код внесёт аналогичную ошибку | Medium | Low (Drop safety net ловит) | Инвариант в спеке + code review |
| `match` бойлерплейт в `send_and_translate` усложняет чтение | Low | Low | Приемлемо, код линейный |

---

## Archive Information

**Archived:** 2026-06-23
**Duration:** < 1 day
**Outcome:** Successfully implemented

### Files Modified
- `src/interactions_handler.rs` — 7 error paths fixed: explicit `match` + `guard.finish_with_error()` replaces bare `?`/`.map_err()?`

### Specs Updated
- `openspec/specs/diagnostics.md` — new invariant ("No Unfinalized Guard on Error Return") + 7 scenarios + expanded split check scenario
