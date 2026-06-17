# Delta: Deployment & Packaging

**Change ID:** `windows-secrets-dir`
**Affects:** `packaging/windows/install.ps1`, `README.*.md`, `.github/workflows/ci.yml`

---

## MODIFIED

### Requirement: Windows Package (zip)

`install.ps1` теперь создаёт директорию `secrets\` внутри `%ProgramData%\inf-splitter\`. Пользователь задаёт API-ключи через файлы `secrets/VAR` вместо WinSW env vars:

```powershell
# Основной способ (рекомендованный):
# Создайте файл %ProgramData%\inf-splitter\secrets\DEEPSEEK_API_KEY
# и поместите в него значение ключа
Restart-Service inf-splitter

# Альтернативный способ (через WinSW, если нужны настоящие env vars):
& "$env:ProgramData\inf-splitter\inf-splitter-service.exe" set DEEPSEEK_API_KEY=sk-...
& "$env:ProgramData\inf-splitter\inf-splitter-service.exe" restart
```

Резолв `${VAR}` в конфиге читает `secrets/VAR` на всех платформах — этот механизм уже реализован в `src/config.rs`.

#### Scenario: Windows install creates secrets dir
- GIVEN `install.ps1` запущен от администратора
- WHEN установка завершается
- THEN `%ProgramData%\inf-splitter\secrets\` существует и готов к использованию

#### Scenario: API key from secrets file
- GIVEN `config.toml` содержит `api_key = "${DEEPSEEK_API_KEY}"`
- WHEN `%ProgramData%\inf-splitter\secrets\DEEPSEEK_API_KEY` содержит `sk-abc123`
- THEN прокси использует `sk-abc123` как API-ключ

#### Scenario: Env var takes precedence over secrets file
- GIVEN и env var `DEEPSEEK_API_KEY=sk-env`, и файл `secrets/DEEPSEEK_API_KEY=sk-file` существуют
- WHEN прокси резолвит `${DEEPSEEK_API_KEY}`
- THEN используется `sk-env` (env var приоритетнее)
