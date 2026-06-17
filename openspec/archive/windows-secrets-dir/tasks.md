# Implementation Tasks: Windows secrets/ директория

**Change ID:** `windows-secrets-dir`

Каждый шаг — RED→GREEN: проверяем, что сейчас неправильно, затем исправляем.

---

## Step 1: install.ps1 — secrets/ директория

- [x] 1.1 **RED** — Проверить: `install.ps1` не создаёт `secrets\` (строка 14 — только `$targetDir` и `$logsDir`)
- [x] 1.2 **GREEN** — Добавить `New-Item -ItemType Directory -Force -Path "$targetDir\secrets" | Out-Null`

---

## Step 2: install.ps1 — финальные подсказки

- [x] 2.1 **RED** — Проверить: строки 38-40 ссылаются на `winsw set` для API-ключей
- [x] 2.2 **GREEN** — Заменить на: «Создайте файл secrets\КЛЮЧ и поместите в него значение ключа» + упомянуть `Restart-Service`

---

## Step 3: README.md (Russian)

- [x] 3.1 **RED** — Проверить: секция Windows показывает только `winsw set` для ключей
- [x] 3.2 **GREEN** — Переписать: secrets/ как основной способ, WinSW как fallback

---

## Step 4: README.en.md + README.zh.md

- [x] 4.1 **RED** — Проверить: английская и китайская версии также ссылаются на `winsw set`
- [x] 4.2 **GREEN** — Синхронизировать с русской версией

---

## Step 5: Релизное тело CI

- [x] 5.1 **RED** — Проверить: `.github/workflows/ci.yml` в теле релиза (Windows-секция) ссылается на `winsw set`
- [x] 5.2 **GREEN** — Заменить на: «Создайте файл secrets\КЛЮЧ и поместите в него значение ключа»

---

## Quality Gate

- [x] `cargo test --locked` — 144 теста, без регрессий
- [x] `cargo fmt --check` — clean
- [x] `cargo clippy` — clean
- [x] README heading counts синхронизированы (11/11/11)

---

## Completion Checklist

- [ ] Все RED→GREEN шаги выполнены
- [ ] Quality gates пройдены
- [ ] Ready for `/openspec-archive`
