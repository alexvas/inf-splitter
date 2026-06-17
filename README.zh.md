[ Русский ](README.md) | [ English ](README.en.md) | **中文**

# inf-splitter

一个轻量级HTTP推理请求路由器：基于模型名称从TOML配置路由到OpenAI和Anthropic兼容的上游。

**主要用途 — 在本地主机上运行。** 服务默认监听 `127.0.0.1:{port}`（端口来自TOML配置，默认3000）。

替代 `anyllm-proxy`：无需LiteLLM YAML、管理界面或通过`/etc/hosts`的SSRF绕过。

## 入口安全（无认证）

**传入代理的请求不进行身份验证。** 网络中任何能够访问服务端口的客户端都可以发送请求。运维人员负责边界安全（网络、反向代理、防火墙）。

认证仅在上游提供商侧生效：如果配置段指定了`api_key`，代理会将其注入上游请求；如果未指定`api_key`，客户端的传入认证头将原样转发。

## 配置

主配置文件：[`config/inf-splitter.toml`](config/inf-splitter.toml)。

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

| 字段 | 描述 |
|------|------|
| `listen_host` | 传入连接的IP地址（默认`127.0.0.1`；Docker使用`0.0.0.0`） |
| `listen_port` | TCP端口（默认3000） |
| `upstream_timeout` | 出站上游请求超时；后缀`s`（秒）或`m`（分钟），例如`15s`、`1m`（默认`5m`） |
| `max_request_body` | 最大传入请求体大小；后缀`k`（KiB）或`m`（MiB），例如`512k`、`2m`（默认`2m`） |
| `[[error_translation]]` | 可选的表格数组，用于替换上游错误响应体。每个表格包含：`status`（HTTP状态码）、`ingress`（可选，用于在响应体中匹配的子字符串）、`egress`（替换文本）。规则按顺序检查，首次匹配生效。如果该段缺失或为空——错误响应体保持不变 |

### `[defaults]` 段

所有提供商的全局令牌限制。单个提供商可以覆盖这些值。

| 字段 | 描述 |
|------|------|
| `max_tokens` | 全局`max_tokens`限制（适用于所有上游，除非被覆盖） |
| `max_output_tokens` | 全局`max_output_tokens`限制（透传，非标准字段；OpenAI兼容上游请用`max_completion_tokens`） |
| `max_completion_tokens` | 全局`max_completion_tokens`限制（OpenAI兼容上游） |

### 提供商段

| 字段 | 描述 |
|------|------|
| `endpoint_openai` | 可选；OpenAI兼容上游的基础URL。设置后，传入的`/openai`请求直接转发到此而不进行转换 |
| `endpoint_anthropic` | 可选；Anthropic兼容上游的基础URL。设置后，传入的`/anthropic`请求直接转发到此而不进行转换 |
| `models` | 单个模型、模型列表或`"default"`（未匹配模型的回退） |
| `api_key` | 可选；`${VAR}`从环境变量或`secrets/VAR`文件解析 |
| `max_tokens` | 可选；限制出站请求中的`max_tokens`。如果客户端未设置或超出——代理注入限制 |
| `max_output_tokens` | 可选；限制`max_output_tokens`（透传，非标准字段；OpenAI兼容上游请用`max_completion_tokens`） |
| `max_completion_tokens` | 可选；限制`max_completion_tokens`（OpenAI兼容上游） |
| `drop_fields` | 可选；从发送上游的请求体中移除的顶级JSON键。形式：平面列表`["a","b"]`或`[section.drop_fields]`带`all = [...]`及`"model" = [...]` |

可以通过`INF_SPLITTER_CONFIG`覆盖配置路径。

### 环境变量

| 变量 | 描述 |
|------|------|
| `INF_SPLITTER_CONFIG` | TOML配置文件路径（默认`config/inf-splitter.toml`） |
| `INF_SPLITTER_LISTEN_HOST` | 传入连接的IP地址（默认`127.0.0.1`；Docker使用`0.0.0.0`） |

### 密钥

```bash
mkdir -p secrets
cp secrets.example/* secrets/
# 编辑 secrets/DEEPSEEK_API_KEY, secrets/MAAS_API_KEY
```

`secrets/`目录在`.gitignore`中 — 不要提交真实密钥。

`${VAR}`解析顺序：环境变量 → `secrets/VAR`文件。

## 路由

```
Claude Code  --POST /v1/messages-->     inf-splitter
OpenAI SDK  --POST /v1/chat/completions-->
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

| 模型 | 段 | 推荐入口 |
|------|-----|----------|
| `gemma4:31b` | `[ollama]` | `POST /v1/chat/completions` |
| `deepseek-v4-pro[1m]`、`deepseek-v4-flash` | `[deepseek]` | `POST /v1/messages` |
| 任何其他 | `[etc]`（`default`） | `POST /v1/chat/completions` |

入口端点指定**传入请求格式和客户端响应格式**。TOML段通过`endpoint_openai`和/或`endpoint_anthropic`指定**目标上游**。当两者都设置时，`/openai`转到`endpoint_openai`，`/anthropic`转到`endpoint_anthropic`（直通）。当只设置一个时，相反的入口通过`anyllm_translate`进行转换。

| 入口 | 端点可用性 | 行为 |
|------|------------|------|
| `/v1/chat/completions` | 设置了`endpoint_openai` | 直通→OpenAI上游 |
| `/v1/chat/completions` | 仅`endpoint_anthropic` | OpenAI→Anthropic→OpenAI |
| `/v1/messages` | 设置了`endpoint_anthropic` | 直通→Anthropic上游 |
| `/v1/messages` | 仅`endpoint_openai` | Anthropic→OpenAI→Anthropic |

### API密钥

| 段 | `api_key` | 行为 |
|-----|-----------|------|
| `[ollama]` | 未设置 | 客户端的传入密钥（Ollama忽略Authorization） |
| `[deepseek]` | `${DEEPSEEK_API_KEY}` | 代理从env/`secrets/`注入密钥 |
| `[etc]` | `${MAAS_API_KEY}` | 代理从env/`secrets/`注入密钥 |

### `[diagnostics]` 段（可选）

控制统计信息收集和请求/响应转储。将NDJSON行写入指定的接收器。默认全部关闭。

```toml
[diagnostics]
# 写入NDJSON统计信息的位置："stderr"（默认）、"stdout"或文件路径。
stats_output = "stderr"

# 写入NDJSON转储的位置："stderr"（默认）、"stdout"或文件路径。
dump_output = "/app/logs/dump.ndjson"

# 统计（每个请求的摘要：模型、持续时间、令牌数、消息分解）：
# "off" — 不收集；"error" — 仅在错误时；"all" — 每个请求。
stats_mode = "error"

# 请求/响应体转储（用于调试；可能很大）：
# "off" — 不转储；"error" — 仅在错误时；"all" — 每个请求。
dump_mode = "off"

# 刷新间隔（可选，例如"10s"、"1m"）。
# 未设置时，每行后刷新。文件输出时有用，
# 以减少磁盘I/O。
flush_period = "10s"
```

在Docker中设置`stats_output = "stderr"`运行时，统计行会出现在`docker logs`中。要写入文件，挂载卷（`- ./logs:/app/logs`）并设置`stats_output = "/app/logs/diagnostics.ndjson"`。`dump_output`同理。

## HTTP API

| 方法 | 路径 | 描述 |
|------|------|------|
| `GET` | `/health` | 就绪探针：`{"status":"ok","upstreams":{...}}`或上游不可用时`{"status":"degraded",...}`（HTTP 503） |
| `GET` | `/v1/models` | 模型列表 |
| `POST` | `/v1/chat/completions` | OpenAI格式；上游通过TOML中的`model`解析 |
| `POST` | `/v1/messages` | Anthropic格式；上游通过TOML中的`model`解析 |

### `GET /v1/models`

返回TOML中明确列出的所有模型ID（不包括`"default"`），按字典顺序排列。

## docker-compose 集成

`Claude CLI`代理使用路由器作为上游Anthropic API：

- `ANTHROPIC_BASE_URL=http://inf-splitter:${PROXY_PORT:-3000}`（网络内）
- 对于通过OpenAI协议的本地模型：`OPENAI_BASE_URL=http://inf-splitter:${PROXY_PORT:-3000}`

挂载配置和密钥。在Docker中设置`INF_SPLITTER_LISTEN_HOST=0.0.0.0`：

```yaml
environment:
  - INF_SPLITTER_LISTEN_HOST=0.0.0.0
volumes:
  - ./inf-splitter/config:/app/config:ro
  - ./inf-splitter/secrets:/app/secrets:ro
```

### 主机访问Ollama

在Docker中，对`[ollama].endpoint`使用`http://host.docker.internal:11434`，并在compose中添加`extra_hosts: host.docker.internal:host-gateway`。

## 构建与运行

### 本地（cargo）

```bash
cd inf-splitter
cp secrets.example/* secrets/
export DEEPSEEK_API_KEY=sk-...   # 或将密钥放入secrets/
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

## 发布

预构建包可在[GitHub Releases](https://github.com/)中获取（每次推送到`main`的CI产物）。

### Linux (.deb)

```bash
sudo dpkg -i inf-splitter_*.deb
```

软件包将二进制文件安装到`/usr/bin/inf-splitter`，配置文件安装到`/etc/inf-splitter/inf-splitter.toml`，环境变量模板安装到`/etc/inf-splitter/environment`，以及systemd服务。

安装后：
1. 编辑`/etc/inf-splitter/inf-splitter.toml` — 配置您的上游
2. 填写`/etc/inf-splitter/environment` — 设置API密钥（格式`VAR=value`，每行一个）
3. 服务已运行：`systemctl status inf-splitter`

```bash
# 更改配置或环境变量后：
sudo systemctl restart inf-splitter

# 日志：
journalctl -u inf-splitter -f
```

### Windows (zip)

从产物中下载`inf-splitter-windows.zip`，解压并以管理员身份运行`install.ps1`：

```powershell
Expand-Archive inf-splitter-windows.zip -DestinationPath C:\temp\inf-splitter
cd C:\temp\inf-splitter\inf-splitter
.\install.ps1
```

脚本创建`%ProgramData%\inf-splitter\`，安装并启动Windows服务。

安装后：
1. 编辑`%ProgramData%\inf-splitter\config.toml`
2. 设置API密钥：在`%ProgramData%\inf-splitter\secrets\`中创建一个以密钥命名的文件，并将值放入其中。例如，对于`${DEEPSEEK_API_KEY}` — 创建文件`secrets\DEEPSEEK_API_KEY`，内容为`sk-...`
3. 重启服务：`Restart-Service inf-splitter`

或者，通过WinSW设置API密钥：`& "$env:ProgramData\inf-splitter\inf-splitter-service.exe" set VAR=value`

```powershell
Get-Service inf-splitter          # 服务状态
Get-EventLog -LogName Application -Source inf-splitter  # 日志
```

## 代码结构

```
src/
├── main.rs      # 入口点，优雅关闭
├── config.rs    # TOML，模型/默认路由，密钥
├── auth.rs      # api_key注入/认证头转发
├── router.rs    # axum路由，/v1/models（openai+anthropic），/health
├── openai.rs    # OpenAI上游 + Anthropic↔OpenAI转换
├── anthropic.rs # Anthropic上游 + OpenAI↔Anthropic转换
├── sse.rs       # 共享SSE工具（解析、格式化、响应）
└── error.rs     # Anthropic API格式的错误
```

## 测试

```bash
env -u RUSTUP_TOOLCHAIN cargo test
```

协议转换集成测试：`tests/protocol_conversion.rs`（模拟上游 + 通过代理的HTTP）。

### Docker冒烟测试

验证镜像构建、挂载配置启动和HTTP端点：

```bash
./scripts/docker-smoke-test.sh
```

变量：`SMOKE_IMAGE`（镜像标签，默认`inf-splitter:smoke-test`）。

## 故障排除

- **Config load failed: secret not found** — 设置环境变量或将`secrets.example/`复制到`secrets/`。
- **llama: Connection refused** — 检查`[llama-local].endpoint`和本地主机上的llama可达性。

## 许可证

本项目根据[GNU General Public License v3.0 or later](LICENSE)（GPL-3.0-or-later）分发。

Rust依赖项列在[THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES)中；常见许可证文本在[licenses/](licenses/)目录中。CI在每次推送时验证此文件。更新`Cargo.lock`时，重新生成列表：

```bash
python3 scripts/generate-third-party-notices.py
```
