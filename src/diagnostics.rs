use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
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
    /// Rotate current file when it exceeds this size (applies to `Sink::File` only).
    pub max_file_size: Option<u64>,
    /// Delete oldest rotated files when total rotated size exceeds this.
    pub max_rotated_size: Option<u64>,
    /// Compress rotated files.
    pub compression: Option<Compression>,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            stats_output: Sink::Stderr,
            dump_output: Sink::Stderr,
            stats_mode: DiagnosticMode::Off,
            dump_mode: DiagnosticMode::Off,
            flush_period: None,
            max_file_size: None,
            max_rotated_size: None,
            compression: None,
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
    /// Write to per-section files derived from this base path.
    /// `diag.ndjson` + section `ollama` → `diag-ollama.ndjson`.
    FilePerSection(PathBuf),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    Zip,
    Bz2,
    #[serde(rename = "7z")]
    SevenZ,
}

/// Handle for sending diagnostic events to the background writer task.
///
/// Cloning is cheap — all clones share the same channel sender.
#[derive(Clone)]
pub struct Diagnostics {
    default_writers: SectionWriters,
    stats_mode: DiagnosticMode,
    dump_mode: DiagnosticMode,
    counter: Arc<AtomicU64>,
    start_secs: u64,
    section_cfg: Option<Arc<SectionConfig>>,
    section_channels: Arc<Mutex<HashMap<String, SectionWriters>>>,
}

/// Per-section writer configuration (stored for lazy writer creation).
struct SectionConfig {
    stats_output: Sink,
    dump_output: Sink,
    flush_period: Option<Duration>,
    max_file_size: Option<u64>,
    max_rotated_size: Option<u64>,
    compression: Option<Compression>,
}

#[derive(Clone)]
struct SectionWriters {
    stats_tx: mpsc::Sender<String>,
    dump_tx: mpsc::Sender<String>,
}

/// Per-request statistics (model, duration, token counts, message breakdown).
#[derive(Debug, Serialize)]
pub struct StatsEvent {
    pub section: String,
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
#[derive(Debug)]
pub struct DumpEvent {
    pub section: String,
    pub request_id: String,
    pub ts: String,
    pub stage: String,
    pub direction: String,
    pub model: String,
    pub headers: Vec<(String, String)>,
    pub body: DumpBody,
    pub status: Option<u16>,
}

impl Serialize for DumpEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let extra = if self.body.is_base64() { 1 } else { 0 };
        let mut s = serializer.serialize_struct("DumpEvent", 10 + extra)?;
        s.serialize_field("section", &self.section)?;
        s.serialize_field("request_id", &self.request_id)?;
        s.serialize_field("ts", &self.ts)?;
        s.serialize_field("stage", &self.stage)?;
        s.serialize_field("direction", &self.direction)?;
        s.serialize_field("model", &self.model)?;
        s.serialize_field("headers", &self.headers)?;
        match &self.body {
            DumpBody::Utf8(v) => s.serialize_field("body", v)?,
            DumpBody::Base64(v) => {
                s.serialize_field("body", v)?;
                s.serialize_field("encoding", "base64")?;
            }
        }
        if let Some(status) = self.status {
            s.serialize_field("status", &status)?;
        }
        s.end()
    }
}

/// Maximum number of raw bytes to base64-encode for a dump of a non-UTF8 body.
pub const MAX_NON_UTF8_DUMP_LEN: usize = 65536;

/// Result of `dump_body_from_bytes`: either valid UTF-8 or base64-encoded binary.
#[derive(Debug)]
pub enum DumpBody {
    Utf8(String),
    Base64(String),
}

impl DumpBody {
    pub(crate) fn is_base64(&self) -> bool {
        matches!(self, Self::Base64(_))
    }
}

impl From<String> for DumpBody {
    fn from(s: String) -> Self {
        DumpBody::Utf8(s)
    }
}

/// Prepare a body string for a `DumpEvent`.
///
/// Valid UTF-8 → `DumpBody::Utf8(string)`. Binary → truncated to
/// `MAX_NON_UTF8_DUMP_LEN`, base64-encoded → `DumpBody::Base64(string)`.
/// Callers should emit `tracing::warn!` on `DumpBody::Base64`.
pub fn dump_body_from_bytes(body: &[u8]) -> DumpBody {
    match std::str::from_utf8(body) {
        Ok(s) => DumpBody::Utf8(s.to_string()),
        Err(_) => {
            let truncated = if body.len() > MAX_NON_UTF8_DUMP_LEN {
                &body[..MAX_NON_UTF8_DUMP_LEN]
            } else {
                body
            };
            let encoded = base64::engine::general_purpose::STANDARD.encode(truncated);
            DumpBody::Base64(encoded)
        }
    }
}

impl Diagnostics {
    pub fn new(config: DiagnosticsConfig) -> Self {
        let (stats_tx, stats_rx) = mpsc::channel(1024);
        let (dump_tx, dump_rx) = mpsc::channel(1024);
        let stats_mode = config.stats_mode.clone();
        let dump_mode = config.dump_mode.clone();

        let per_section = matches!(config.stats_output, Sink::FilePerSection(_))
            || matches!(config.dump_output, Sink::FilePerSection(_));
        let section_cfg = per_section.then(|| {
            Arc::new(SectionConfig {
                stats_output: config.stats_output.clone(),
                dump_output: config.dump_output.clone(),
                flush_period: config.flush_period,
                max_file_size: config.max_file_size,
                max_rotated_size: config.max_rotated_size,
                compression: config.compression.clone(),
            })
        });

        // Default writer loops: never used when per_section is true (events
        // route to per-section writers instead). Fall back to stderr so idle
        // loops don't open a spurious file.
        let default_sink = |sink: Sink| -> Sink {
            match sink {
                Sink::FilePerSection(_) => Sink::Stderr,
                other => other,
            }
        };

        let stats_handle = {
            let cfg = RotatingWriterConfig {
                sink: default_sink(config.stats_output),
                flush_period: config.flush_period,
                max_file_size: config.max_file_size,
                max_rotated_size: config.max_rotated_size,
                compression: config.compression.clone(),
            };
            tokio::task::spawn_blocking(move || {
                writer_loop(stats_rx, cfg);
            })
        };
        let dump_handle = {
            let cfg = RotatingWriterConfig {
                sink: default_sink(config.dump_output),
                flush_period: config.flush_period,
                max_file_size: config.max_file_size,
                max_rotated_size: config.max_rotated_size,
                compression: config.compression,
            };
            tokio::task::spawn_blocking(move || {
                writer_loop(dump_rx, cfg);
            })
        };
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
            default_writers: SectionWriters { stats_tx, dump_tx },
            stats_mode,
            dump_mode,
            counter: Arc::new(AtomicU64::new(0)),
            start_secs: epoch_secs(),
            section_cfg,
            section_channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a no-op Diagnostics for tests (no background writer task).
    #[cfg(test)]
    pub fn new_noop() -> Self {
        let (stats_tx, _rx) = mpsc::channel(1);
        let (dump_tx, _rx) = mpsc::channel(1);
        Self {
            default_writers: SectionWriters { stats_tx, dump_tx },
            stats_mode: DiagnosticMode::Off,
            dump_mode: DiagnosticMode::Off,
            counter: Arc::new(AtomicU64::new(0)),
            start_secs: epoch_secs(),
            section_cfg: None,
            section_channels: Arc::new(Mutex::new(HashMap::new())),
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

    /// Derive a per-section file path from the base diagnostics path.
    ///
    /// `diag.ndjson` + `ollama` → `diag-ollama.ndjson`
    /// `dump.ndjson` + `deepseek` → `dump-deepseek.ndjson`
    fn section_path(base: &Sink, section: &str) -> Sink {
        match base {
            Sink::File(path) | Sink::FilePerSection(path) => {
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                let ext = path.extension().unwrap_or_default().to_string_lossy();
                let sectioned = if ext.is_empty() {
                    format!("{}-{}", stem, section)
                } else {
                    format!("{}-{}.{}", stem, section, ext)
                };
                Sink::File(path.with_file_name(sectioned))
            }
            Sink::Stderr => Sink::Stderr,
            Sink::Stdout => Sink::Stdout,
        }
    }

    /// Look up or create per-section writer channels.
    fn get_or_create_section_writers(&self, cfg: &SectionConfig, section: &str) -> SectionWriters {
        let mut map = self.section_channels.lock().unwrap();
        if let Some(writers) = map.get(section) {
            return writers.clone();
        }
        let writers = Self::spawn_section_writer(cfg, section);
        map.insert(section.to_string(), writers.clone());
        writers
    }

    /// Spawn background writer loops for a single section.
    fn spawn_section_writer(cfg: &SectionConfig, section: &str) -> SectionWriters {
        let (stats_tx, stats_rx) = mpsc::channel(1024);
        let (dump_tx, dump_rx) = mpsc::channel(1024);

        let stats_sink = Self::section_path(&cfg.stats_output, section);
        let dump_sink = Self::section_path(&cfg.dump_output, section);
        let section_owned = section.to_string();

        let stats_handle = {
            let writer_cfg = RotatingWriterConfig {
                sink: stats_sink,
                flush_period: cfg.flush_period,
                max_file_size: cfg.max_file_size,
                max_rotated_size: cfg.max_rotated_size,
                compression: cfg.compression.clone(),
            };
            tokio::task::spawn_blocking(move || {
                writer_loop(stats_rx, writer_cfg);
            })
        };
        let dump_handle = {
            let writer_cfg = RotatingWriterConfig {
                sink: dump_sink,
                flush_period: cfg.flush_period,
                max_file_size: cfg.max_file_size,
                max_rotated_size: cfg.max_rotated_size,
                compression: cfg.compression.clone(),
            };
            tokio::task::spawn_blocking(move || {
                writer_loop(dump_rx, writer_cfg);
            })
        };

        let s1 = section_owned.clone();
        tokio::spawn(async move {
            if let Err(e) = stats_handle.await {
                tracing::error!(section = %s1, "stats writer panicked: {:?}", e);
            }
        });
        tokio::spawn(async move {
            if let Err(e) = dump_handle.await {
                tracing::error!(section = %section_owned, "dump writer panicked: {:?}", e);
            }
        });

        SectionWriters { stats_tx, dump_tx }
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
        if let Some(ref cfg) = self.section_cfg {
            let tx = self
                .get_or_create_section_writers(cfg, &event.section)
                .stats_tx
                .clone();
            let _ = tx.try_send(json);
        } else {
            let _ = self.default_writers.stats_tx.try_send(json);
        }
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
        if let Some(ref cfg) = self.section_cfg {
            let tx = self
                .get_or_create_section_writers(cfg, &event.section)
                .dump_tx
                .clone();
            let _ = tx.try_send(json);
        } else {
            let _ = self.default_writers.dump_tx.try_send(json);
        }
    }

    /// Record a request dump (ingress or egress).
    ///
    /// `headers` comes from the request HeaderMap; `body` accepts `DumpBody` or
    /// plain `String` (always UTF-8).
    #[allow(clippy::too_many_arguments)]
    pub fn record_request_dump(
        &self,
        request_id: &str,
        section: &str,
        stage: &str,
        model: &str,
        headers: &axum::http::HeaderMap,
        body: impl Into<DumpBody>,
        status: Option<u16>,
        is_error: bool,
    ) {
        self.record_dump(
            &DumpEvent {
                section: section.to_string(),
                request_id: request_id.to_string(),
                ts: ts_string(),
                stage: stage.into(),
                direction: "request".into(),
                model: model.to_string(),
                headers: headers
                    .iter()
                    .filter_map(|(k, v)| {
                        v.to_str()
                            .ok()
                            .map(|val| (k.as_str().to_string(), val.to_string()))
                    })
                    .collect(),
                body: body.into(),
                status,
            },
            is_error,
        );
    }

    /// Record an egress response dump.
    #[allow(clippy::too_many_arguments)]
    pub fn record_response_dump(
        &self,
        request_id: &str,
        section: &str,
        model: &str,
        headers: Vec<(String, String)>,
        body: impl Into<DumpBody>,
        status: u16,
        is_error: bool,
    ) {
        self.record_dump(
            &DumpEvent {
                section: section.to_string(),
                request_id: request_id.to_string(),
                ts: ts_string(),
                stage: "egress".into(),
                direction: "response".into(),
                model: model.to_string(),
                headers,
                body: body.into(),
                status: Some(status),
            },
            is_error,
        );
    }
}

// ── background writer ────────────────────────────────────────────

fn open_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// Generate a rotated filename: `{stem}-YYYY-MM-DD-NNN.{ext}`.
/// Scans the directory for existing rotated files to determine the next sequence
/// number for today.
fn rotate_filename(path: &std::path::Path) -> PathBuf {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy();

    let today = chrono_now();
    let prefix = format!("{stem}-{today}-");

    // Find highest sequence number for today
    let mut max_seq = 0u32;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(&prefix) {
                continue;
            }
            // Try uncompressed (.ext) and compressed (.ext.zip, .ext.gz, etc.)
            let suffixes: &[&str] = if ext.is_empty() {
                &[]
            } else {
                &[&*ext, "gz", "zip", "bz2", "7z"]
            };
            for suffix in suffixes {
                let full_suffix = if *suffix == ext.as_ref() {
                    format!(".{}", suffix)
                } else {
                    format!(".{}.{}", ext, suffix)
                };
                if name.ends_with(&full_suffix) {
                    let inner = &name[prefix.len()..name.len() - full_suffix.len()];
                    if let Ok(n) = inner.parse::<u32>() {
                        max_seq = max_seq.max(n);
                    }
                    break;
                }
            }
        }
    }

    let seq = max_seq + 1;
    path.with_file_name(format!("{prefix}{seq:03}.{ext}"))
}

/// Return today's date as "YYYY-MM-DD".
fn chrono_now() -> String {
    // Avoid pulling in chrono — compute from SystemTime
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // days since epoch
    let days = secs / 86400;
    // Approximate year/month/day from days since 1970-01-01.
    // This is a simplified calendar — good enough for filenames.
    let (y, m, d) = civil_from_days(days as i64 + 719468); // 719468 = days from 0000-01-01 to 1970-01-01
    format!("{y:04}-{m:02}-{d:02}")
}

/// Convert days since 0000-03-01 to (year, month, day).
/// Based on Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Compress a file in a background thread. Returns the compressed path on success.
fn compress_file(src: PathBuf, compression: &Compression) -> tokio::task::JoinHandle<()> {
    let comp = compression.clone();
    tokio::task::spawn_blocking(move || {
        let ext = match &comp {
            Compression::Zip => "zip",
            Compression::Bz2 => "bz2",
            Compression::SevenZ => "7z",
        };
        let dst = src.with_extension(format!(
            "{}.{}",
            src.extension().unwrap_or_default().to_string_lossy(),
            ext
        ));

        match &comp {
            Compression::Zip => {
                let input = match std::fs::File::open(&src) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!(src = %src.display(), error = %e, "compress: open failed");
                        return;
                    }
                };
                let output = match std::fs::File::create(&dst) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!(dst = %dst.display(), error = %e, "compress: create failed");
                        return;
                    }
                };
                let mut zipw = zip::ZipWriter::new(output);
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                let entry_name = src.file_name().unwrap_or_default().to_string_lossy();
                if let Err(e) = zipw.start_file(entry_name.as_ref(), options) {
                    tracing::error!(dst = %dst.display(), error = %e, "compress: zip start failed");
                    let _ = std::fs::remove_file(&dst);
                    return;
                }
                if let Err(e) = std::io::copy(&mut std::io::BufReader::new(input), &mut zipw) {
                    tracing::error!(src = %src.display(), error = %e, "compress: zip write failed");
                    let _ = std::fs::remove_file(&dst);
                    return;
                }
                if let Err(e) = zipw.finish() {
                    tracing::error!(dst = %dst.display(), error = %e, "compress: zip finish failed");
                    let _ = std::fs::remove_file(&dst);
                    return;
                }
                if let Err(e) = std::fs::remove_file(&src) {
                    tracing::error!(src = %src.display(), error = %e, "compress: remove original failed");
                }
                tracing::debug!(src = %src.display(), dst = %dst.display(), "rotated file compressed");
            }
            Compression::Bz2 | Compression::SevenZ => {
                tracing::warn!(
                    compression = ?comp,
                    "compression not yet implemented, leaving uncompressed"
                );
            }
        }
    })
}

/// Delete oldest rotated files until total size ≤ `max_size`.
/// Rotated files match `{stem}-*{ext}[.{comp_ext}]`.
fn cleanup_rotated_files(path: &Path, max_size: u64) {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy();
    let stem_prefix = format!("{}-", stem);

    let mut rotated: Vec<(PathBuf, u64)> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let name = p.file_name()?.to_string_lossy();
                let current = path.file_name()?.to_string_lossy();
                if name != current && name.starts_with(&stem_prefix) {
                    // Accept both uncompressed (.ndjson) and compressed (.ndjson.gz, etc.)
                    if name.ends_with(&*ext) || name.contains(&format!(".{}.", ext)) {
                        let meta = e.metadata().ok()?;
                        return Some((p, meta.len()));
                    }
                }
                None
            })
            .collect(),
        Err(_) => return,
    };

    // Sort by name (which encodes date+seq), oldest first
    rotated.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));

    let total: u64 = rotated.iter().map(|(_, s)| s).sum();
    let mut excess = total.saturating_sub(max_size);

    for (path, size) in &rotated {
        if excess == 0 {
            break;
        }
        if let Err(e) = std::fs::remove_file(path) {
            tracing::error!(path = %path.display(), error = %e, "cleanup: failed to remove");
        } else {
            excess = excess.saturating_sub(*size);
        }
    }
}

/// Rotation-aware writer for diagnostics files.
struct RotatingWriter {
    path: PathBuf,
    writer: BufWriter<std::fs::File>,
    bytes_written: u64,
    max_file_size: Option<u64>,
    compression: Option<Compression>,
}

impl RotatingWriter {
    fn new(
        path: PathBuf,
        max_file_size: Option<u64>,
        compression: Option<Compression>,
    ) -> std::io::Result<Self> {
        let max_file_size = max_file_size.filter(|&s| s > 0);
        let file = open_file(&path)?;
        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            writer: BufWriter::new(file),
            bytes_written,
            max_file_size,
            compression,
        })
    }

    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let line_bytes = line.as_bytes();
        // +1 for newline
        let total = line_bytes.len() as u64 + 1;

        if let Some(limit) = self.max_file_size {
            if self.bytes_written + total > limit {
                self.rotate()?;
            }
        }

        writeln!(self.writer, "{line}")?;
        self.bytes_written += total;
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.writer.flush()?;

        let rotated_path = rotate_filename(&self.path);
        std::fs::rename(&self.path, &rotated_path)?;

        // Spawn compression in background if configured
        if let Some(ref comp) = self.compression {
            let handle = compress_file(rotated_path, comp);
            tokio::spawn(async move {
                if let Err(e) = handle.await {
                    tracing::error!("compression task panicked: {:?}", e);
                }
            });
        }

        // Open new file
        let file = open_file(&self.path)?;
        self.writer = BufWriter::new(file);
        self.bytes_written = 0;
        Ok(())
    }
}

/// Configuration passed to the writer loop.
struct RotatingWriterConfig {
    sink: Sink,
    flush_period: Option<Duration>,
    max_file_size: Option<u64>,
    max_rotated_size: Option<u64>,
    compression: Option<Compression>,
}

fn writer_loop(receiver: mpsc::Receiver<String>, config: RotatingWriterConfig) {
    match config.sink {
        Sink::File(path) | Sink::FilePerSection(path) => {
            // Startup: cleanup old files and rotate current if needed
            if let Some(max_rotated) = config.max_rotated_size {
                cleanup_rotated_files(&path, max_rotated);
            }
            let mut rw = match RotatingWriter::new(path, config.max_file_size, config.compression) {
                Ok(rw) => rw,
                Err(e) => {
                    tracing::error!(error = %e, "failed to open rotating writer, falling back to stderr");
                    writer_loop_stderr(receiver, config.flush_period);
                    return;
                }
            };
            // Pre-rotate if existing file already exceeds limit
            if let Some(limit) = config.max_file_size {
                if rw.bytes_written >= limit {
                    if let Err(e) = rw.rotate() {
                        tracing::error!(error = %e, "startup rotate failed");
                    }
                }
            }
            writer_loop_rotating(receiver, rw, config.flush_period);
        }
        Sink::Stderr | Sink::Stdout => {
            writer_loop_stream(receiver, &config.sink, config.flush_period);
        }
    }
}

fn writer_loop_rotating(
    mut receiver: mpsc::Receiver<String>,
    mut rw: RotatingWriter,
    flush_period: Option<Duration>,
) {
    let period = match flush_period {
        Some(p) => p,
        None => {
            while let Some(line) = receiver.blocking_recv() {
                if let Err(e) = rw.write_line(&line) {
                    tracing::error!(error = %e, "diagnostics write error, falling back to stderr");
                    return writer_loop_stderr(receiver, None);
                }
                if let Err(e) = rw.flush() {
                    tracing::error!(error = %e, "diagnostics flush error, falling back to stderr");
                    return writer_loop_stderr(receiver, None);
                }
            }
            return;
        }
    };

    let mut last_flush = Instant::now();
    while let Some(line) = receiver.blocking_recv() {
        if let Err(e) = rw.write_line(&line) {
            tracing::error!(error = %e, "diagnostics write error, falling back to stderr");
            return writer_loop_stderr(receiver, Some(period));
        }
        if last_flush.elapsed() >= period {
            if let Err(e) = rw.flush() {
                tracing::error!(error = %e, "diagnostics flush error, falling back to stderr");
                return writer_loop_stderr(receiver, Some(period));
            }
            last_flush = Instant::now();
        }
    }
    if let Err(e) = rw.flush() {
        tracing::error!(error = %e, "diagnostics final flush error");
    }
}

fn writer_loop_stream(
    mut receiver: mpsc::Receiver<String>,
    sink: &Sink,
    flush_period: Option<Duration>,
) {
    let inner: Box<dyn Write + Send> = match sink {
        Sink::Stderr => Box::new(std::io::stderr()),
        Sink::Stdout => Box::new(std::io::stdout()),
        Sink::File(_) | Sink::FilePerSection(_) => unreachable!(),
    };
    let mut writer = BufWriter::new(inner);

    let period = match flush_period {
        Some(p) => p,
        None => {
            while let Some(line) = receiver.blocking_recv() {
                if let Err(e) = writeln!(writer, "{line}") {
                    tracing::error!(error = %e, "diagnostics write error");
                }
                if let Err(e) = writer.flush() {
                    tracing::error!(error = %e, "diagnostics flush error");
                }
            }
            return;
        }
    };

    let mut last_flush = Instant::now();
    while let Some(line) = receiver.blocking_recv() {
        if let Err(e) = writeln!(writer, "{line}") {
            tracing::error!(error = %e, "diagnostics write error");
        }
        if last_flush.elapsed() >= period {
            if let Err(e) = writer.flush() {
                tracing::error!(error = %e, "diagnostics flush error");
            }
            last_flush = Instant::now();
        }
    }
    if let Err(e) = writer.flush() {
        tracing::error!(error = %e, "diagnostics final flush error");
    }
}

fn writer_loop_stderr(receiver: mpsc::Receiver<String>, flush_period: Option<Duration>) {
    writer_loop_stream(receiver, &Sink::Stderr, flush_period);
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
    if std::str::from_utf8(body).is_err() {
        return Some(serde_json::json!([{
            "role": "?",
            "parts": [non_utf8_part_detail(body.len())]
        }]));
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    messages_detail_from_value(&v)
}

pub fn non_utf8_part_detail(len: usize) -> Value {
    serde_json::json!({"type": "non-utf8", "len": len})
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
            section: "test".into(),
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
            section: "test".into(),
            request_id: "1234-0".into(),
            ts: "1000".into(),
            stage: "ingress".into(),
            direction: "request".into(),
            model: "claude".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: DumpBody::Utf8(r#"{"model":"claude","max_tokens":100}"#.into()),
            status: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"stage\":\"ingress\""));
        assert!(json.contains("\"direction\":\"request\""));
    }

    #[test]
    fn dump_event_with_encoding_serializes() {
        let event = DumpEvent {
            section: "test".into(),
            request_id: "1234-0".into(),
            ts: "1000".into(),
            stage: "ingress".into(),
            direction: "request".into(),
            model: "model".into(),
            headers: vec![],
            body: DumpBody::Base64("aGVsbG8=".into()),
            status: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"encoding\":\"base64\""));
        assert!(json.contains("\"body\":\"aGVsbG8=\""));
    }

    #[test]
    fn dump_event_without_encoding_skips_field() {
        let event = DumpEvent {
            section: "test".into(),
            request_id: "1234-0".into(),
            ts: "1000".into(),
            stage: "ingress".into(),
            direction: "request".into(),
            model: "model".into(),
            headers: vec![],
            body: DumpBody::Utf8("hello".into()),
            status: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("encoding"));
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

    #[test]
    fn messages_detail_from_bytes_non_utf8() {
        let body = vec![0xFF, 0xFE, 0x00, 0x01];
        let detail = messages_detail_from_bytes(&body);
        assert!(detail.is_some(), "should return detail for non-UTF8");
        let detail = detail.unwrap();
        let parts = detail[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "non-utf8");
        assert_eq!(parts[0]["len"], 4);
    }

    // --- civil_from_days ---

    #[test]
    fn civil_from_days_epoch() {
        // 1970-01-01 = epoch reference (day 0)
        assert_eq!(civil_from_days(719468), (1970, 1, 1));
        // Day 1 after epoch
        assert_eq!(civil_from_days(719469), (1970, 1, 2));
        // Internal consistency: consecutive days produce consecutive dates
        let (y1, m1, d1) = civil_from_days(730000);
        let (y2, m2, d2) = civil_from_days(730001);
        assert!(y2 >= y1 && (y2 > y1 || m2 >= m1) && (y2 > y1 || m2 > m1 || d2 > d1));
    }

    // --- dump_body_from_bytes ---

    #[test]
    fn dump_body_from_bytes_utf8() {
        let result = dump_body_from_bytes(b"hello world");
        match result {
            DumpBody::Utf8(s) => assert_eq!(s, "hello world"),
            DumpBody::Base64(_) => panic!("expected Utf8, got Base64"),
        }
    }

    #[test]
    fn dump_body_from_bytes_non_utf8_base64() {
        let bytes = vec![0xFF, 0xFE, 0x00];
        let result = dump_body_from_bytes(&bytes);
        match result {
            DumpBody::Base64(s) => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(&s)
                    .expect("must be valid base64");
                assert_eq!(decoded, bytes);
            }
            DumpBody::Utf8(_) => panic!("expected Base64, got Utf8"),
        }
    }

    #[test]
    fn dump_body_from_bytes_truncates_large_non_utf8() {
        let bytes = vec![0xFFu8; 70 * 1024];
        let result = dump_body_from_bytes(&bytes);
        match result {
            DumpBody::Base64(s) => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(&s)
                    .expect("valid base64");
                assert_eq!(decoded.len(), MAX_NON_UTF8_DUMP_LEN);
            }
            DumpBody::Utf8(_) => panic!("expected Base64, got Utf8"),
        }
    }
}
