# Proposal: Windows — хранение секретов через файлы secrets/

**Change ID:** `windows-secrets-dir`
**Created:** 2026-06-17
**Status:** Implementation Complete
**Completed:** 2026-06-17
**Archived:** 2026-06-17

---

## Problem Statement

Сейчас пользователь под Windows должен задавать API-ключи через WinSW:

```powershell
& "$env:ProgramData\inf-splitter\inf-splitter-service.exe" set DEEPSEEK_API_KEY=sk-...
& "$env:ProgramData\inf-splitter\inf-splitter-service.exe" restart
```

Этот подход плох для обычного пользователя:
- Нужно помнить синтаксис WinSW (сетевые сервисы, запуск из-под админа)
- Непривычно — консольная команда вместо редактирования файла
- Не видно всех переменных разом (в отличие от `.env`-файла или директории)

## Подходы к хранению секретов под Windows

| Подход | Пример | Плюсы | Минусы |
|--------|--------|-------|--------|
| **Файлы `secrets/`** | `echo sk-... > secrets/DEEPSEEK_API_KEY` | Уже работает в коде, единообразие с Linux, просто | Надо объяснить про `${VAR}` |
| WinSW env vars | `winsw set VAR=value` | Нативные переменные окружения | Сложный синтаксис, не видны в файловой системе |
| `.env` файл | `DEEPSEEK_API_KEY=sk-...` в одном файле | Привычный формат | Надо парсить, новый код |
| Windows Credential Manager | `cmdkey /add` | Системное шифрование | Сложный API, нет кроссплатформенности |
| Админка/веб-интерфейс | GUI на localhost | Удобно для non-tech | Огромный scope, не нужно |

**Вывод:** файлы `secrets/` — оптимальный вариант. Код уже умеет читать `secrets/VAR` (Linux и Windows). Остаётся только создать директорию при установке и обновить документацию.

## Proposed Solution

1. `install.ps1` создаёт `secrets\` директорию при установке
2. `README` (и версии для релизного CI) рекомендуют `secrets/` как основной способ
3. WinSW env vars остаются как fallback (env vars имеют приоритет), но документация выводит их на второй план

## Scope

### In Scope
- `install.ps1`: создать `secrets\` директорию
- `install.ps1`: обновить подсказки при установке — «Создайте файл secrets\КЛЮЧ и поместите в него значение» вместо `winsw set`
- `README.md` (+ `.en.md`, `.zh.md`): обновить секцию Windows — secrets/ как основной способ
- `.github/workflows/ci.yml`: обновить инструкции в теле релиза
- `packaging/windows/config.toml`: возможно добавить комментарий про `secrets/`

### Out of Scope
- Изменения в коде (резолв `${VAR}` уже работает)
- `.env` парсинг
- Windows Credential Manager
- GUI / админка

## Impact Analysis

| Component | Change Required | Details |
|-----------|-----------------|---------|
| `packaging/windows/install.ps1` | Yes | Создание `secrets\`, новые подсказки |
| `README.md` × 3 | Yes | Секция Windows — secrets/ как primary |
| `.github/workflows/ci.yml` | Yes | Тело релиза — замена `winsw set` на `echo ... > secrets\` |
| Код (`src/`) | No | Уже работает |

## Success Criteria

- [ ] После `install.ps1` существует `%ProgramData%\inf-splitter\secrets\`
- [ ] Пользователь может задать ключ через `echo sk-... > %ProgramData%\inf-splitter\secrets\DEEPSEEK_API_KEY`
- [ ] После `Restart-Service inf-splitter` ключ подхватывается
- [ ] README (все три языка) обновлены
- [ ] Релизное тело CI обновлено
