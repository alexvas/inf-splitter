# inf-splitter

Тонкий HTTP-роутер для запросов инференса: маршрутизация по модели из TOML-конфигурации на OpenAI- и Anthropic-совместимые upstream.

**Основное предназначение — запуск в контейнере.** Сервис по умолчанию слушает `0.0.0.0:{port}` (порт из TOML, по умолчанию 3000), чтобы быть доступным внутри Docker-сети без дополнительной настройки bind.

Заменяет `anyllm-proxy`: без LiteLLM YAML, admin UI и SSRF-обходов через `/etc/hosts`.

## Безопасность ingress (no-auth)

**Входящие запросы к прокси не аутентифицируются.** Любой клиент в сети, имеющий доступ к порту сервиса, может отправлять запросы. Защиту на границе (сеть Docker, reverse proxy, firewall) обеспечивает оператор.

Аутентификация применяется только на стороне upstream-провайдеров: если в секции конфигурации задан `api_key`, прокси подставляет его в upstream-запрос; если `api_key` не задан, входящие auth-заголовки клиента передаются как есть.

## Конфигурация

Основной файл: [`config/inf-splitter.toml`](config/inf-splitter.toml).

```toml
upstream_timeout = "3m"
max_request_body = "2m"

[defaults]
max_tokens = 4096
max_completion_tokens = 8192

[ollama]
endpoint_openai = "http://127.0.0.1:11434"
models = "gemma4:31b"

[deepseek]
endpoint_anthropic = "https://api.deepseek.com/anthropic"
api_key = "${DEEPSEEK_API_KEY}"
models = ["deepseek-v4-pro[1m]", "deepseek-v4-flash"]

[etc]
endpoint_openai = "https://api.modelarts-maas.com/openai/v1"
api_key = "${MAAS_API_KEY}"
models = "default"
```

| Поле | Описание |
|------|----------|
| `port` | TCP-порт; сервис слушает `0.0.0.0:{port}` (по умолчанию 3000) |
| `upstream_timeout` | Таймаут исходящих запросов к upstream; суффиксы `s` (секунды) или `m` (минуты), напр. `15s`, `1m` (по умолчанию `5m`) |
| `max_request_body` | Максимальный размер входящего тела запроса; суффиксы `k` (KiB) или `m` (MiB), напр. `512k`, `2m` (по умолчанию `2m`) |
| `body_too_large_hint_statuses` | Опциональный список HTTP-статусов (числа), при которых к ошибке добавляется подсказка `Try reducing context size...` (по умолчанию `[413]`, пустой список = подсказка не добавляется) |

### Секция `[defaults]`

Глобальные лимиты токенов для всех провайдеров. Конкретный провайдер может переопределить их своим значением.

| Поле | Описание |
|------|----------|
| `max_tokens` | Глобальный лимит `max_tokens` (действует для всех upstream, если не переопределён) |
| `max_output_tokens` | Глобальный лимит `max_output_tokens` (Anthropic/Gemini-совместимые upstream) |
| `max_completion_tokens` | Глобальный лимит `max_completion_tokens` (OpenAI-совместимые upstream) |

### Секции провайдеров

| Поле секции | Описание |
|-------------|----------|
| `endpoint_openai` | Опционально; base URL OpenAI-совместимого upstream. Если задан, входящие запросы `/openai` идут сюда без конверсии |
| `endpoint_anthropic` | Опционально; base URL Anthropic-совместимого upstream. Если задан, входящие запросы `/anthropic` идут сюда без конверсии |
| `models` | Одна модель, список моделей или `"default"` (fallback для несматчившихся) |
| `api_key` | Опционально; `${VAR}` резолвится из env или файла `secrets/VAR` |
| `max_tokens` | Опционально; лимит на `max_tokens` в исходящем запросе. Если клиент не задал или превысил — прокси подставляет лимит |
| `max_output_tokens` | Опционально; лимит на `max_output_tokens` (Anthropic/Gemini-совместимые upstream) |
| `max_completion_tokens` | Опционально; лимит на `max_completion_tokens` (OpenAI-совместимые upstream) |

Путь к конфигу можно переопределить через `INF_SPLITTER_CONFIG`.

### Переменные окружения

| Переменная | Описание |
|------------|----------|
| `INF_SPLITTER_CONFIG` | Путь к TOML-конфигу (по умолчанию `config/inf-splitter.toml`) |
| `DUMP_ON_ERROR` | `1`/`true`/`yes` — при ошибке апстрима писать в stderr одну JSON-строку с HTTP-статусом, телом ошибки, моделью, размером запроса и разбором сообщений (текст: слова/символы; картинки: байты; tool_use: имя/размер аргументов) |

### Секреты

```bash
mkdir -p secrets
cp secrets.example/* secrets/
# отредактируйте secrets/DEEPSEEK_API_KEY, secrets/MAAS_API_KEY
```

Каталог `secrets/` в `.gitignore` — не коммитьте реальные ключи.

Порядок резолва `${VAR}`: переменная окружения → файл `secrets/VAR`.

## Маршрутизация

```
Claude Code  --POST /openai/v1/messages-->     inf-splitter
            --POST /anthropic/v1/messages-->
                         |
              model + ingress protocol
                         |
         +---------------+---------------+
         |                               |
    OPENAI section                  ANTHROPIC section
         |                               |
    OpenAI upstream               Anthropic upstream
  (/v1/chat/completions)           (/v1/messages)
```

| Модель | Секция | Рекомендуемый ingress |
|--------|--------|------------------------|
| `gemma4:31b` | `[ollama]` | `POST /openai/v1/messages` |
| `deepseek-v4-pro[1m]`, `deepseek-v4-flash` | `[deepseek]` | `POST /anthropic/v1/messages` |
| любая другая | `[etc]` (`default`) | `POST /openai/v1/messages` |

Ingress endpoint задаёт **формат входящего запроса и ответа клиенту**. Секция TOML задаёт **целевой upstream** через `endpoint_openai` и/или `endpoint_anthropic`. Если заданы оба — `/openai` идёт на `endpoint_openai`, `/anthropic` — на `endpoint_anthropic` (passthrough). Если задан только один — встречный ingress конвертируется через `anyllm_translate`.

| Ingress | Наличие endpoint | Поведение |
|---------|-------------------|-----------|
| `/openai/v1/messages` | `endpoint_openai` задан | passthrough → OpenAI upstream |
| `/openai/v1/messages` | только `endpoint_anthropic` | OpenAI → Anthropic → OpenAI |
| `/anthropic/v1/messages` | `endpoint_anthropic` задан | passthrough → Anthropic upstream |
| `/anthropic/v1/messages` | только `endpoint_openai` | Anthropic → OpenAI → Anthropic |

### API-ключи

| Секция | `api_key` | Поведение |
|--------|-----------|-----------|
| `[ollama]` | не задан | Входящий ключ клиента (Ollama игнорирует Authorization) |
| `[deepseek]` | `${DEEPSEEK_API_KEY}` | Прокси подставляет ключ из env/`secrets/` |
| `[etc]` | `${MAAS_API_KEY}` | Прокси подставляет ключ из env/`secrets/` |

### Секция `[diagnostics]` (опционально)

Управляет сбором статистики и дампом запросов/ответов. Пишет строки NDJSON в указанный sink. По умолчанию всё выключено.

```toml
[diagnostics]
# Куда писать NDJSON: "stderr" (по умолчанию), "stdout", или путь к файлу.
output = "stderr"

# Статистика (сводка по каждому запросу: модель, длительность, количество токенов, разбор сообщений):
# "off" — не собирать; "error" — только при ошибках; "all" — каждый запрос.
stats = "error"

# Дамп тел запросов и ответов (для отладки, может быть объёмным):
# "off" — не дампить; "error" — только при ошибках; "all" — каждый запрос.
dump = "off"
```

При запуске в Docker с `output = "stderr"` строки NDJSON попадают в `docker logs`. Для записи в файл замонтируйте volume (`- ./logs:/app/logs`) и укажите `output = "/app/logs/diagnostics.ndjson"`.

## HTTP API

| Метод | Путь | Описание |
|-------|------|----------|
| `GET` | `/health` | Readiness probe: `{"status":"ok","upstreams":{...}}` или `{"status":"degraded",...}` (HTTP 503) при недоступных upstream |
| `GET` | `/openai/v1/models` | OpenAI-совместимый список моделей |
| `GET` | `/anthropic/v1/models` | Anthropic-совместимый список моделей |
| `POST` | `/openai/v1/messages` | OpenAI-формат; upstream по `model` из TOML |
| `POST` | `/anthropic/v1/messages` | Anthropic-формат; upstream по `model` из TOML |

### `GET /openai/v1/models` и `GET /anthropic/v1/models`

Возвращают все явно перечисленные в TOML model id (без `"default"`), в лексикографическом порядке.

## Интеграция с docker-compose

Контейнер `claude` использует роутер как upstream Anthropic API:

- `ANTHROPIC_BASE_URL=http://inf-splitter:${PROXY_PORT:-3000}/anthropic` (внутри Docker-сети)
- Для локальных моделей через OpenAI-протокол: `http://inf-splitter:${PROXY_PORT}/openai`

Смонтируйте конфиг и секреты:

```yaml
volumes:
  - ./inf-splitter/config:/app/config:ro
  - ./inf-splitter/secrets:/app/secrets:ro
```

### Доступ к Ollama на хосте

В Docker для `[ollama].endpoint` используйте `http://host.docker.internal:11434` и `extra_hosts: host.docker.internal:host-gateway` в compose.

## Сборка и запуск

### Локально (cargo)

```bash
cd inf-splitter
cp secrets.example/* secrets/
export DEEPSEEK_API_KEY=sk-...   # или положите ключи в secrets/
export MAAS_API_KEY=sk-...
cargo run
```

### Docker

```bash
docker build -t inf-splitter .
docker run --rm \
  -v "$PWD/config:/app/config:ro" \
  -v "$PWD/secrets:/app/secrets:ro" \
  inf-splitter
```

## Структура кода

```
src/
├── main.rs      # точка входа, graceful shutdown
├── config.rs    # TOML, маршрутизация по model/default, секреты
├── auth.rs      # подстановка api_key / проброс auth-заголовков
├── router.rs    # маршруты axum, /v1/models (openai+anthropic), /health
├── openai.rs    # OpenAI upstream + конверсия Anthropic↔OpenAI
├── anthropic.rs # Anthropic upstream + конверсия OpenAI↔Anthropic
├── sse.rs       # общие утилиты для SSE (парсинг, форматирование, ответы)
└── error.rs     # ошибки в формате Anthropic API
```

## Тесты

```bash
env -u RUSTUP_TOOLCHAIN cargo test
```

Интеграционные тесты конверсии протоколов: `tests/protocol_conversion.rs` (mock upstream + HTTP через прокси).

### Docker smoke test

Проверяет сборку образа, старт с монтированным конфигом и HTTP endpoints:

```bash
./scripts/docker-smoke-test.sh
```

Переменные: `SMOKE_IMAGE` (тег образа, по умолчанию `inf-splitter:smoke-test`).

## Устранение неполадок

- **Config load failed: secret not found** — задайте env-переменную или скопируйте `secrets.example/` в `secrets/`.
- **Ollama: Connection refused** — проверьте `[ollama].endpoint` и доступность Ollama с хоста/контейнера.

## Лицензия

Проект распространяется под [GNU General Public License v3.0 or later](LICENSE) (GPL-3.0-or-later).

Зависимости Rust перечислены в [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES); тексты распространённых лицензий — в каталоге [licenses/](licenses/). После обновления `Cargo.lock` перегенерируйте список:

```bash
python3 scripts/generate-third-party-notices.py
```
