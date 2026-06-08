use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;

/// Parsed from the optional `[diagnostics]` TOML section.
#[derive(Debug, Clone)]
pub struct DiagnosticsConfig {
    pub stats_output: Sink,
    pub dump_output: Sink,
    pub stats_mode: DiagnosticMode,
    pub dump_mode: DiagnosticMode,
    /// When set, flush to disk at most this often (e.g. `10s`).
    /// When `None`, flush after every line.
    pub flush_period: Option<Duration>,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            stats_output: Sink::Stderr,
            dump_output: Sink::Stderr,
            stats_mode: DiagnosticMode::Off,
            dump_mode: DiagnosticMode::Off,
            flush_period: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticMode {
    Off,
    Error,
    All,
}

#[derive(Debug, Clone)]
pub enum Sink {
    Stderr,
    Stdout,
    File(PathBuf),
}

/// Handle for sending diagnostic events to the background writer task.
///
/// Cloning is cheap — all clones share the same channel sender.
#[derive(Clone)]
pub struct Diagnostics {
    stats_tx: mpsc::Sender<String>,
    dump_tx: mpsc::Sender<String>,
    stats_mode: DiagnosticMode,
    dump_mode: DiagnosticMode,
    counter: Arc<AtomicU64>,
    start_secs: u64,
}

/// Per-request statistics (model, duration, token counts, message breakdown).
#[derive(Debug, Serialize)]
pub struct StatsEvent {
    pub request_id: String,
    pub ts: String,
    pub direction: String,
    pub model: String,
    pub upstream: String,
    pub status: u16,
    pub duration_ms: u64,
    pub request_size_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_size_bytes: Option<usize>,
    pub streaming: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_messages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_detail_ingress: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages_detail_egress: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Raw request or response body dump.
#[derive(Debug, Serialize)]
pub struct DumpEvent {
    pub request_id: String,
    pub ts: String,
    pub stage: String,
    pub direction: String,
    pub model: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub status: Option<u16>,
}

impl Diagnostics {
    pub fn new(config: DiagnosticsConfig) -> Self {
        let (stats_tx, stats_rx) = mpsc::channel(1024);
        let (dump_tx, dump_rx) = mpsc::channel(1024);
        let stats_mode = config.stats_mode.clone();
        let dump_mode = config.dump_mode.clone();

        let stats_handle = tokio::task::spawn_blocking(move || {
            writer_loop(stats_rx, config.stats_output, config.flush_period);
        });
        let dump_handle = tokio::task::spawn_blocking(move || {
            writer_loop(dump_rx, config.dump_output, config.flush_period);
        });
        // Supervisors: log panics from background writers.
        tokio::spawn(async move {
            if let Err(e) = stats_handle.await {
                tracing::error!("stats writer panicked: {:?}", e);
            }
        });
        tokio::spawn(async move {
            if let Err(e) = dump_handle.await {
                tracing::error!("dump writer panicked: {:?}", e);
            }
        });

        Self {
            stats_tx,
            dump_tx,
            stats_mode,
            dump_mode,
            counter: Arc::new(AtomicU64::new(0)),
            start_secs: epoch_secs(),
        }
    }

    /// Create a no-op Diagnostics for tests (no background writer task).
    #[cfg(test)]
    pub fn new_noop() -> Self {
        let (stats_tx, _rx) = mpsc::channel(1);
        let (dump_tx, _rx) = mpsc::channel(1);
        Self {
            stats_tx,
            dump_tx,
            stats_mode: DiagnosticMode::Off,
            dump_mode: DiagnosticMode::Off,
            counter: Arc::new(AtomicU64::new(0)),
            start_secs: epoch_secs(),
        }
    }

    pub fn stats_mode(&self) -> &DiagnosticMode {
        &self.stats_mode
    }

    pub fn dump_mode(&self) -> &DiagnosticMode {
        &self.dump_mode
    }

    /// Returns true if stats recording is active (mode is `Error` or `All`).
    /// Use this to skip expensive stats computation when diagnostics are off.
    pub fn stats_enabled(&self) -> bool {
        !matches!(self.stats_mode, DiagnosticMode::Off)
    }

    /// Returns true if dump recording is active (mode is `Error` or `All`).
    /// Use this to skip expensive dump serialization when diagnostics are off.
    pub fn dump_enabled(&self) -> bool {
        !matches!(self.dump_mode, DiagnosticMode::Off)
    }

    /// Generate a new unique request id: `{startup_secs}-{counter}`.
    pub fn new_request_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{}-{}", self.start_secs, n)
    }

    /// Non-blocking send of a stats line. Respects `stats_mode`:
    /// - `Off` → nothing
    /// - `Error` → only if `event.error` is Some
    /// - `All` → always
    ///
    /// Uses `try_send` and silently drops the event when the channel is full.
    /// This is intentional: diagnostics are best-effort and must never block
    /// request processing or add backpressure to the data path.
    pub fn record_stats(&self, event: &StatsEvent) {
        match &self.stats_mode {
            DiagnosticMode::Off => return,
            DiagnosticMode::Error if event.error.is_none() => return,
            _ => {}
        }
        let Ok(json) = serde_json::to_string(event) else {
            return;
        };
        let _ = self.stats_tx.try_send(json);
    }

    /// Non-blocking send of a dump line. Respects `dump_mode`.
    /// `is_error` indicates whether this dump is for an error request.
    ///
    /// Same best-effort semantics as `record_stats`: uses `try_send` and
    /// silently drops the event when the channel is full. This is intentional
    /// to avoid backpressure on request processing.
    pub fn record_dump(&self, event: &DumpEvent, is_error: bool) {
        match &self.dump_mode {
            DiagnosticMode::Off => return,
            DiagnosticMode::Error if !is_error => return,
            _ => {}
        }
        let Ok(json) = serde_json::to_string(event) else {
            return;
        };
        let _ = self.dump_tx.try_send(json);
    }
}

// ── background writer ────────────────────────────────────────────

fn open_sink(sink: &Sink) -> Box<dyn Write + Send> {
    match sink {
        Sink::Stderr => Box::new(std::io::stderr()),
        Sink::Stdout => Box::new(std::io::stdout()),
        Sink::File(path) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                Ok(f) => Box::new(f),
                Err(e) => {
                    tracing::error!(
                        path = %path.display(),
                        error = %e,
                        "failed to open diagnostics file, falling back to stderr"
                    );
                    Box::new(std::io::stderr())
                }
            }
        }
    }
}

fn writer_loop(mut receiver: mpsc::Receiver<String>, sink: Sink, flush_period: Option<Duration>) {
    let inner = open_sink(&sink);
    let mut writer = BufWriter::new(inner);

    let period = match flush_period {
        Some(p) => p,
        None => {
            // Flush every line.
            while let Some(line) = receiver.blocking_recv() {
                if let Err(e) = writeln!(writer, "{line}") {
                    tracing::error!(error = %e, "diagnostics write error, reopening sink");
                    writer = BufWriter::new(open_sink(&sink));
                }
                if let Err(e) = writer.flush() {
                    tracing::error!(error = %e, "diagnostics flush error, reopening sink");
                    writer = BufWriter::new(open_sink(&sink));
                }
            }
            return;
        }
    };

    let mut last_flush = Instant::now();
    while let Some(line) = receiver.blocking_recv() {
        if let Err(e) = writeln!(writer, "{line}") {
            tracing::error!(error = %e, "diagnostics write error, reopening sink");
            writer = BufWriter::new(open_sink(&sink));
        }
        if last_flush.elapsed() >= period {
            if let Err(e) = writer.flush() {
                tracing::error!(error = %e, "diagnostics periodic flush error, reopening sink");
                writer = BufWriter::new(open_sink(&sink));
            }
            last_flush = Instant::now();
        }
    }
    // Final flush on shutdown.
    if let Err(e) = writer.flush() {
        tracing::error!(error = %e, "diagnostics final flush error");
    }
}

// ── helpers ──────────────────────────────────────────────────────

pub fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn ts_string() -> String {
    epoch_secs().to_string()
}

// ── message detail builders (moved from lib.rs) ──────────────────

use serde_json::Value;

/// Build per-message detail from an Anthropic `MessageCreateRequest`.
pub fn anthropic_messages_detail(req: &anyllm_translate::anthropic::MessageCreateRequest) -> Value {
    use anyllm_translate::anthropic::Content;
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|msg| {
            let parts = match &msg.content {
                Content::Text(text) => vec![text_part_detail(text)],
                Content::Blocks(blocks) => blocks.iter().map(anthropic_block_part).collect(),
            };
            serde_json::json!({"role": msg.role, "parts": parts})
        })
        .collect();
    serde_json::json!(messages)
}

/// Build per-message detail from an OpenAI `ChatCompletionRequest`.
pub fn openai_messages_detail(req: &anyllm_translate::openai::ChatCompletionRequest) -> Value {
    use anyllm_translate::openai::{ChatContent, ChatContentPart};
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|msg| {
            let mut parts: Vec<Value> = match &msg.content {
                None => Vec::new(),
                Some(ChatContent::Text(text)) => vec![text_part_detail(text)],
                Some(ChatContent::Parts(content_parts)) => content_parts
                    .iter()
                    .map(|p| match p {
                        ChatContentPart::Text { text } => text_part_detail(text),
                        ChatContentPart::ImageUrl { image_url } => serde_json::json!({
                            "type": "image_url",
                            "url_chars": image_url.url.len()
                        }),
                        ChatContentPart::InputAudio { input_audio } => serde_json::json!({
                            "type": "audio",
                            "bytes": input_audio.data.len()
                        }),
                        ChatContentPart::File { file } => serde_json::json!({
                            "type": "file",
                            "bytes": file.file_data.as_ref().map(|d| d.len()).unwrap_or(0)
                        }),
                    })
                    .collect(),
            };
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    parts.push(serde_json::json!({
                        "type": "tool_call",
                        "name": tc.function.name,
                        "args_chars": tc.function.arguments.len()
                    }));
                }
            }
            serde_json::json!({"role": msg.role, "parts": parts})
        })
        .collect();
    serde_json::json!(messages)
}

/// Best-effort message detail from raw JSON bytes (passthrough paths).
pub fn messages_detail_from_bytes(body: &[u8]) -> Option<Value> {
    let v: Value = serde_json::from_slice(body).ok()?;
    messages_detail_from_value(&v)
}

/// Like `messages_detail_from_bytes` but works on an already-parsed `Value`,
/// avoiding a second deserialization when the body is already in memory.
pub fn messages_detail_from_value(body: &Value) -> Option<Value> {
    let arr = body.get("messages")?.as_array()?;
    Some(messages_detail_from_array(arr))
}

fn messages_detail_from_array(arr: &[Value]) -> Value {
    let detail: Vec<Value> = arr
        .iter()
        .map(|msg| {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
            let content = msg.get("content");
            let parts: Vec<Value> = match content {
                Some(Value::String(text)) => vec![text_part_detail(text)],
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| {
                        let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match ty {
                            "text" => text_part_detail(
                                item.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                            ),
                            "image" => {
                                let source = item.get("source");
                                let bytes = source
                                    .and_then(|s| s.get("data"))
                                    .and_then(|d| d.as_str())
                                    .map(|d| d.len())
                                    .unwrap_or(0);
                                let media = source
                                    .and_then(|s| s.get("media_type"))
                                    .and_then(|m| m.as_str());
                                let mut p = serde_json::json!({"type": "image", "bytes": bytes});
                                if let Some(m) = media {
                                    p["media_type"] = serde_json::json!(m);
                                }
                                p
                            }
                            "image_url" => {
                                let url = item
                                    .get("image_url")
                                    .and_then(|iu| iu.get("url"))
                                    .and_then(|u| u.as_str());
                                serde_json::json!({
                                    "type": "image_url",
                                    "url_chars": url.map(|u| u.len()).unwrap_or(0)
                                })
                            }
                            "tool_use" => {
                                let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                let input_bytes =
                                    item.get("input").map(|i| i.to_string().len()).unwrap_or(0);
                                serde_json::json!({
                                    "type": "tool_use",
                                    "name": name,
                                    "input_bytes": input_bytes
                                })
                            }
                            _ => serde_json::json!({"type": ty}),
                        }
                    })
                    .collect(),
                _ => Vec::new(),
            };
            serde_json::json!({"role": role, "parts": parts})
        })
        .collect();
    serde_json::json!(detail)
}

fn text_part_detail(text: &str) -> Value {
    serde_json::json!({
        "type": "text",
        "chars": text.chars().count(),
        "words": text.split_whitespace().count()
    })
}

fn anthropic_block_part(block: &anyllm_translate::anthropic::ContentBlock) -> Value {
    use anyllm_translate::anthropic::ContentBlock;
    match block {
        ContentBlock::Text { text } => text_part_detail(text),
        ContentBlock::Image { source } => {
            let bytes = source.data.as_ref().map(|d| d.len()).unwrap_or(0);
            serde_json::json!({
                "type": "image",
                "bytes": bytes,
                "media_type": source.media_type
            })
        }
        ContentBlock::Document { source, .. } => {
            serde_json::json!({
                "type": "document",
                "bytes": 0,
                "media_type": source.media_type
            })
        }
        ContentBlock::ToolUse { name, input, .. } => {
            serde_json::json!({
                "type": "tool_use",
                "name": name,
                "input_bytes": input.to_string().len()
            })
        }
        ContentBlock::ToolResult { content, .. } => match content {
            Some(tc) => {
                let txt = match tc {
                    anyllm_translate::anthropic::ToolResultContent::Text(s) => s.clone(),
                    anyllm_translate::anthropic::ToolResultContent::Blocks(_) => "(blocks)".into(),
                };
                text_part_detail(&txt)
            }
            None => serde_json::json!({"type": "tool_result"}),
        },
        ContentBlock::Thinking { thinking, .. } => text_part_detail(thinking),
        ContentBlock::RedactedThinking { data } => {
            serde_json::json!({"type": "redacted_thinking", "bytes": data.len()})
        }
        // Catch-all for forward-compat (ServerToolUse, WebSearchToolResult,
        // WebFetchToolResult, Unknown added in anyllm_translate 0.9.7+).
        _ => serde_json::json!({"type": "unknown"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_mode_default_is_off() {
        let config = DiagnosticsConfig::default();
        assert!(matches!(config.stats_mode, DiagnosticMode::Off));
        assert!(matches!(config.dump_mode, DiagnosticMode::Off));
        assert!(matches!(config.stats_output, Sink::Stderr));
        assert!(matches!(config.dump_output, Sink::Stderr));
    }

    #[test]
    fn request_id_is_unique_and_monotonic() {
        let diag = Diagnostics::new_noop();
        let id1 = diag.new_request_id();
        let id2 = diag.new_request_id();
        assert_ne!(id1, id2);
        let n1: u64 = id1.split('-').last().unwrap().parse().unwrap();
        let n2: u64 = id2.split('-').last().unwrap().parse().unwrap();
        assert!(n2 > n1);
    }

    #[test]
    fn stats_event_serializes_cleanly() {
        let event = StatsEvent {
            request_id: "1234-0".into(),
            ts: "1000".into(),
            direction: "openai->openai".into(),
            model: "gpt-4".into(),
            upstream: "https://api.openai.com".into(),
            status: 200,
            duration_ms: 150,
            request_size_bytes: 1024,
            response_size_bytes: Some(512),
            streaming: false,
            input_messages: Some(2),
            max_tokens: Some(4096),
            messages_detail_ingress: None,
            messages_detail_egress: Some(
                serde_json::json!([{"role":"user","parts":[{"type":"text","chars":5,"words":1}]}]),
            ),
            error: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"direction\":\"openai->openai\""));
        assert!(json.contains("\"messages_detail_egress\""));
        assert!(!json.contains("messages_detail_ingress"));
    }

    #[test]
    fn dump_event_serializes_cleanly() {
        let event = DumpEvent {
            request_id: "1234-0".into(),
            ts: "1000".into(),
            stage: "ingress".into(),
            direction: "request".into(),
            model: "claude".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: r#"{"model":"claude","max_tokens":100}"#.into(),
            status: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"stage\":\"ingress\""));
        assert!(json.contains("\"direction\":\"request\""));
    }

    // --- message detail builders ---

    #[test]
    fn anthropic_messages_detail_text_only() {
        let req: anyllm_translate::anthropic::MessageCreateRequest =
            serde_json::from_value(serde_json::json!({
                "model": "claude",
                "max_tokens": 100,
                "messages": [
                    {"role": "user", "content": "hello there"},
                    {"role": "assistant", "content": [{"type": "text", "text": "hi back"}]}
                ]
            }))
            .unwrap();
        let detail = anthropic_messages_detail(&req);
        let msgs = detail.as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["parts"][0]["type"], "text");
        assert_eq!(msgs[0]["parts"][0]["words"], 2);
    }

    #[test]
    fn anthropic_messages_detail_image_and_tool_use() {
        let req: anyllm_translate::anthropic::MessageCreateRequest =
            serde_json::from_value(serde_json::json!({
                "model": "claude",
                "max_tokens": 100,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "describe"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aaaa"}},
                        {"type": "tool_use", "id": "t1", "name": "search", "input": {"q": "rust"}}
                    ]
                }]
            }))
            .unwrap();
        let detail = anthropic_messages_detail(&req);
        let parts = detail[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image");
        assert_eq!(parts[1]["bytes"], 4);
        assert_eq!(parts[2]["type"], "tool_use");
        assert_eq!(parts[2]["name"], "search");
    }

    #[test]
    fn openai_messages_detail_text_only() {
        let req: anyllm_translate::openai::ChatCompletionRequest =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-4",
                "messages": [
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "content": "world"}
                ]
            }))
            .unwrap();
        let detail = openai_messages_detail(&req);
        let msgs = detail.as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["parts"][0]["type"], "text");
        assert_eq!(msgs[0]["parts"][0]["words"], 1);
    }

    #[test]
    fn openai_messages_detail_with_tool_calls() {
        let req: anyllm_translate::openai::ChatCompletionRequest =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-4",
                "messages": [{
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"London\"}"}
                    }]
                }]
            }))
            .unwrap();
        let detail = openai_messages_detail(&req);
        let parts = detail[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "tool_call");
        assert_eq!(parts[0]["name"], "get_weather");
        assert!(parts[0]["args_chars"].as_u64().unwrap() > 0);
    }

    #[test]
    fn messages_detail_from_bytes_anthropic_shape() {
        let body = br#"{
            "model": "claude",
            "max_tokens": 100,
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}, {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "bbbb"}}]},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
            ]
        }"#;
        let detail = messages_detail_from_bytes(body).unwrap();
        let msgs = detail.as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        let parts = msgs[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["words"], 1);
        assert_eq!(parts[1]["type"], "image");
        assert_eq!(parts[1]["bytes"], 4);
    }

    #[test]
    fn messages_detail_from_bytes_openai_shape() {
        let body = br#"{
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "hello world"},
                {"role": "assistant", "content": [{"type": "text", "text": "response"}, {"type": "image_url", "image_url": {"url": "https://ex.com/i.png"}}]}
            ]
        }"#;
        let detail = messages_detail_from_bytes(body).unwrap();
        let msgs = detail.as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["parts"][0]["type"], "text");
        assert_eq!(msgs[1]["parts"][1]["type"], "image_url");
        assert!(msgs[1]["parts"][1]["url_chars"].as_u64().unwrap() > 0);
    }

    #[test]
    fn messages_detail_from_bytes_missing_field() {
        assert!(messages_detail_from_bytes(br#"{"model":"x"}"#).is_none());
        assert!(messages_detail_from_bytes(br#"{"messages":"not_array"}"#).is_none());
        assert!(messages_detail_from_bytes(b"garbage").is_none());
    }
}
