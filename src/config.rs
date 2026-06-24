use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::HeaderValue;
use serde::Deserialize;
use thiserror::Error;

use crate::diagnostics::{DiagnosticMode, DiagnosticsConfig, Sink};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAi,
    Anthropic,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenAi => write!(f, "OPENAI"),
            Self::Anthropic => write!(f, "ANTHROPIC"),
        }
    }
}

/// Fields to drop from the request body before forwarding to upstream.
///
/// Deserialized from TOML as either a flat list `["a","b"]` or a per-model table
/// `{ all = [...], "model-x" = [...] }` where `"all"` is a reserved base key.
#[derive(Debug, Clone)]
pub enum DropFields {
    All(HashSet<String>),
    PerModel {
        all: HashSet<String>,
        by_model: HashMap<String, HashSet<String>>,
    },
}

impl Default for DropFields {
    fn default() -> Self {
        DropFields::All(HashSet::new())
    }
}

impl DropFields {
    /// Merge `all` + model-specific fields for the given model name.
    pub fn for_model(&self, model: &str) -> HashSet<String> {
        match self {
            DropFields::All(fields) => fields.clone(),
            DropFields::PerModel { all, by_model } => {
                let mut merged = all.clone();
                if let Some(extra) = by_model.get(model) {
                    merged.extend(extra.iter().cloned());
                }
                merged
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            DropFields::All(fields) => fields.is_empty(),
            DropFields::PerModel { all, by_model } => all.is_empty() && by_model.is_empty(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DropFieldsRaw {
    Flat(Vec<String>),
    PerModel(BTreeMap<String, Vec<String>>),
}

impl From<DropFieldsRaw> for DropFields {
    fn from(raw: DropFieldsRaw) -> Self {
        match raw {
            DropFieldsRaw::Flat(list) => DropFields::All(list.into_iter().collect()),
            DropFieldsRaw::PerModel(map) => {
                let mut all = HashSet::new();
                let mut by_model = HashMap::new();
                for (key, fields) in map {
                    if key == "all" {
                        all = fields.into_iter().collect();
                    } else {
                        by_model.insert(key, fields.into_iter().collect());
                    }
                }
                DropFields::PerModel { all, by_model }
            }
        }
    }
}

impl<'de> Deserialize<'de> for DropFields {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        DropFieldsRaw::deserialize(deserializer).map(DropFields::from)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RouteTarget {
    pub section: String,
    pub endpoint_openai: Option<String>,
    pub endpoint_anthropic: Option<String>,
    pub endpoint_interactions: Option<String>,
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub max_completion_tokens: Option<u32>,
    pub model_names: HashSet<String>,
    pub drop_fields: DropFields,
    pub proxy: Option<String>,
    pub proxy_limit: Option<usize>,
    pub control_clean_all: Option<String>,
    pub control_extend_lifetime: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderSection {
    pub(crate) name: String,
    pub(crate) endpoint_openai: Option<String>,
    pub(crate) endpoint_anthropic: Option<String>,
    pub(crate) endpoint_interactions: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) max_tokens: Option<u32>,
    pub(crate) max_output_tokens: Option<u32>,
    pub(crate) max_completion_tokens: Option<u32>,
    pub(crate) model_names: HashSet<String>,
    pub(crate) drop_fields: DropFields,
    pub(crate) proxy: Option<String>,
    pub(crate) proxy_limit: Option<usize>,
    pub(crate) control_clean_all: Option<String>,
    pub(crate) control_extend_lifetime: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorTranslationRule {
    pub status: u16,
    #[serde(default)]
    pub ingress: Option<String>,
    pub egress: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub upstream_timeout: Duration,
    pub max_request_body: usize,
    pub error_translation: Vec<ErrorTranslationRule>,
    pub interactions_session_store: Option<String>,
    pub diagnostics: DiagnosticsConfig,
    default_max_tokens: Option<u32>,
    default_max_output_tokens: Option<u32>,
    default_max_completion_tokens: Option<u32>,
    pub(crate) sections: HashMap<String, ProviderSection>,
    model_routes: HashMap<String, String>,
    default_section: Option<String>,
}

const DEFAULT_UPSTREAM_TIMEOUT: &str = "5m";
const DEFAULT_MAX_REQUEST_BODY: &str = "2m";

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SinkRaw {
    Simple(String),
    PerSection { per_section: String },
}

#[derive(Debug, Deserialize)]
struct DiagnosticsConfigRaw {
    stats_output: Option<SinkRaw>,
    dump_output: Option<SinkRaw>,
    stats_mode: Option<DiagnosticMode>,
    dump_mode: Option<DiagnosticMode>,
    flush_period: Option<String>,
    max_file_size: Option<String>,
    max_rotated_size: Option<String>,
    compression: Option<crate::diagnostics::Compression>,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    listen_host: Option<String>,
    listen_port: Option<u16>,
    upstream_timeout: Option<String>,
    max_request_body: Option<String>,
    defaults: Option<DefaultConfig>,
    #[serde(default)]
    error_translation: Vec<ErrorTranslationRule>,
    #[serde(default)]
    interactions_session_store: Option<String>,
    diagnostics: Option<DiagnosticsConfigRaw>,
    #[serde(flatten)]
    providers: HashMap<String, ProviderConfigRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultConfig {
    max_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    max_completion_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ProviderConfigRaw {
    endpoint_openai: Option<String>,
    endpoint_anthropic: Option<String>,
    endpoint_interactions: Option<String>,
    models: ModelsField,
    api_key: Option<String>,
    max_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    max_completion_tokens: Option<u32>,
    #[serde(default)]
    drop_fields: DropFields,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default)]
    proxy_limit: Option<String>,
    #[serde(default)]
    control_clean_all: Option<String>,
    #[serde(default)]
    control_extend_lifetime: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ModelsField {
    Single(String),
    List(Vec<String>),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    FileNotFound(String),
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("provider section {name}: {message}")]
    Provider { name: String, message: String },
    #[error("duplicate model {model} in sections {first} and {second}")]
    DuplicateModel {
        model: String,
        first: String,
        second: String,
    },
    #[error("multiple default provider sections: {first} and {second}")]
    MultipleDefaults { first: String, second: String },
    #[error("drop_fields references model {model} not in section {section}")]
    UnknownDropModel { section: String, model: String },
    #[error(
        "no provider section defines models = \"default\" and model {0} is not listed elsewhere"
    )]
    UnroutableModel(String),
    #[error("secret not found for {0}: set env var or secrets/{0} file")]
    SecretNotFound(String),
    #[error("config must define at least one provider section")]
    NoProviders,
    #[error("invalid listen port: {0}")]
    Port(String),
    #[error("invalid duration {value}: {message}")]
    InvalidDuration { value: String, message: String },
    #[error("invalid byte size {value}: {message}")]
    InvalidByteSize { value: String, message: String },
    #[error("invalid api_key in section {section}: {message}")]
    InvalidApiKey { section: String, message: String },
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        let path = config_path();
        let raw = fs::read_to_string(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ConfigError::FileNotFound(path.display().to_string())
            } else {
                ConfigError::Io(err)
            }
        })?;

        Self::load_from_str(&raw)
    }

    pub fn load_from_str(raw: &str) -> Result<Self, ConfigError> {
        let file: FileConfig = toml::from_str(raw)?;
        Self::from_file_config(file)
    }

    fn from_file_config(file: FileConfig) -> Result<Self, ConfigError> {
        if file.providers.is_empty() {
            return Err(ConfigError::NoProviders);
        }

        let listen_addr = resolve_listen_addr(file.listen_host.as_deref(), file.listen_port)?;
        let diagnostics = match file.diagnostics {
            Some(raw) => DiagnosticsConfig {
                stats_output: sink_from_raw(raw.stats_output),
                dump_output: sink_from_raw(raw.dump_output),
                stats_mode: raw.stats_mode.unwrap_or(DiagnosticMode::Off),
                dump_mode: raw.dump_mode.unwrap_or(DiagnosticMode::Off),
                flush_period: raw
                    .flush_period
                    .as_deref()
                    .map(parse_duration_field)
                    .transpose()?,
                max_file_size: raw
                    .max_file_size
                    .as_deref()
                    .map(parse_byte_size_field)
                    .transpose()?
                    .map(|v| v as u64),
                max_rotated_size: raw
                    .max_rotated_size
                    .as_deref()
                    .map(parse_byte_size_field)
                    .transpose()?
                    .map(|v| v as u64),
                compression: raw.compression,
            },
            None => DiagnosticsConfig::default(),
        };
        let upstream_timeout = parse_duration_field(
            file.upstream_timeout
                .as_deref()
                .unwrap_or(DEFAULT_UPSTREAM_TIMEOUT),
        )?;
        let max_request_body = parse_byte_size_field(
            file.max_request_body
                .as_deref()
                .unwrap_or(DEFAULT_MAX_REQUEST_BODY),
        )?;

        let defaults = file.defaults.unwrap_or(DefaultConfig {
            max_tokens: None,
            max_output_tokens: None,
            max_completion_tokens: None,
        });

        let mut sections = HashMap::new();
        let mut model_routes = HashMap::new();
        let mut default_section: Option<String> = None;

        for (name, raw_section) in file.providers {
            let endpoint_openai = raw_section
                .endpoint_openai
                .map(|e| e.trim().trim_end_matches('/').to_string())
                .filter(|e| !e.is_empty());
            let endpoint_anthropic = raw_section
                .endpoint_anthropic
                .map(|e| e.trim().trim_end_matches('/').to_string())
                .filter(|e| !e.is_empty());
            let endpoint_interactions = raw_section
                .endpoint_interactions
                .map(|e| e.trim().trim_end_matches('/').to_string())
                .filter(|e| !e.is_empty());

            if endpoint_openai.is_none()
                && endpoint_anthropic.is_none()
                && endpoint_interactions.is_none()
            {
                return Err(ConfigError::Provider {
                    name,
                    message: "at least one of endpoint_openai, endpoint_anthropic, or endpoint_interactions must be set"
                        .to_string(),
                });
            }

            let proxy_limit = match raw_section.proxy_limit.as_deref() {
                Some(val) => Some(parse_byte_size_field(val)?),
                None => None,
            };

            let api_key = match raw_section.api_key {
                Some(value) => {
                    let resolved = resolve_secret(&value)?;
                    // Validate as legal HTTP header value
                    if resolved.is_empty() {
                        return Err(ConfigError::InvalidApiKey {
                            section: name.clone(),
                            message: "api_key must not be empty".to_string(),
                        });
                    }
                    if let Err(e) = HeaderValue::from_str(&resolved) {
                        return Err(ConfigError::InvalidApiKey {
                            section: name.clone(),
                            message: format!("api_key contains invalid HTTP header bytes: {e}"),
                        });
                    }
                    Some(resolved)
                }
                None => None,
            };

            let (is_default, model_names) = parse_models(&name, raw_section.models)?;

            // Validate per-model drop_fields keys against the section's model list
            // (only for non-default sections where we have a concrete model set).
            if !is_default {
                if let DropFields::PerModel { by_model, .. } = &raw_section.drop_fields {
                    for key in by_model.keys() {
                        if !model_names.contains(key) {
                            return Err(ConfigError::UnknownDropModel {
                                section: name.clone(),
                                model: key.clone(),
                            });
                        }
                    }
                }
            }

            if is_default {
                if let Some(existing) = &default_section {
                    return Err(ConfigError::MultipleDefaults {
                        first: existing.clone(),
                        second: name.clone(),
                    });
                }
                default_section = Some(name.clone());
            } else {
                for model in &model_names {
                    if let Some(existing) = model_routes.insert(model.clone(), name.clone()) {
                        return Err(ConfigError::DuplicateModel {
                            model: model.clone(),
                            first: existing,
                            second: name.clone(),
                        });
                    }
                }
            }

            sections.insert(
                name.clone(),
                ProviderSection {
                    name,
                    endpoint_openai,
                    endpoint_anthropic,
                    endpoint_interactions,
                    api_key,
                    max_tokens: raw_section.max_tokens,
                    max_output_tokens: raw_section.max_output_tokens,
                    max_completion_tokens: raw_section.max_completion_tokens,
                    model_names,
                    drop_fields: raw_section.drop_fields,
                    proxy: raw_section.proxy,
                    proxy_limit,
                    control_clean_all: raw_section.control_clean_all,
                    control_extend_lifetime: raw_section.control_extend_lifetime,
                },
            );
        }

        Ok(Self {
            listen_addr,
            diagnostics,
            upstream_timeout,
            max_request_body,
            error_translation: file.error_translation,
            interactions_session_store: file.interactions_session_store,
            default_max_tokens: defaults.max_tokens,
            default_max_output_tokens: defaults.max_output_tokens,
            default_max_completion_tokens: defaults.max_completion_tokens,
            sections,
            model_routes,
            default_section,
        })
    }

    pub fn resolve_route(&self, model: &str) -> Result<RouteTarget, ConfigError> {
        let section_name = self
            .model_routes
            .get(model)
            .cloned()
            .or_else(|| self.default_section.clone())
            .ok_or_else(|| ConfigError::UnroutableModel(model.to_string()))?;

        let section = self
            .sections
            .get(&section_name)
            .ok_or_else(|| ConfigError::Provider {
                name: section_name.clone(),
                message: "internal: section referenced by route does not exist".to_string(),
            })?;

        Ok(RouteTarget {
            section: section.name.clone(),
            endpoint_openai: section.endpoint_openai.clone(),
            endpoint_anthropic: section.endpoint_anthropic.clone(),
            endpoint_interactions: section.endpoint_interactions.clone(),
            api_key: section.api_key.clone(),
            max_tokens: section.max_tokens.or(self.default_max_tokens),
            max_output_tokens: section.max_output_tokens.or(self.default_max_output_tokens),
            max_completion_tokens: section
                .max_completion_tokens
                .or(self.default_max_completion_tokens),
            model_names: section.model_names.clone(),
            drop_fields: section.drop_fields.clone(),
            proxy: section.proxy.clone(),
            proxy_limit: section.proxy_limit,
            control_clean_all: section.control_clean_all.clone(),
            control_extend_lifetime: section.control_extend_lifetime.clone(),
        })
    }

    /// Deterministic, lexicographically sorted union of all explicitly configured model ids.
    pub fn sorted_model_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.model_routes.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Unique upstream endpoints for health checks.
    pub fn upstream_endpoints(&self) -> Vec<(String, String, bool)> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for section in self.sections.values() {
            let eps = [
                section.endpoint_openai.as_deref(),
                section.endpoint_anthropic.as_deref(),
            ];
            for ep in eps.into_iter().flatten() {
                if seen.insert(ep.to_string()) {
                    result.push((section.name.clone(), ep.to_string(), false));
                }
            }
            if let Some(ref ep) = section.endpoint_interactions {
                if seen.insert(ep.clone()) {
                    result.push((section.name.clone(), ep.clone(), true));
                }
            }
        }
        result
    }

    #[cfg(test)]
    pub fn from_model_routes(model_routes: HashMap<String, String>) -> Self {
        Self {
            listen_addr: "127.0.0.1:3000".parse().expect("test listen addr"),
            diagnostics: DiagnosticsConfig::default(),
            upstream_timeout: parse_duration(DEFAULT_UPSTREAM_TIMEOUT).expect("default timeout"),
            max_request_body: parse_byte_size(DEFAULT_MAX_REQUEST_BODY)
                .expect("default body limit"),
            error_translation: Vec::new(),
            interactions_session_store: None,
            default_max_tokens: None,
            default_max_output_tokens: None,
            default_max_completion_tokens: None,
            sections: HashMap::new(),
            model_routes,
            default_section: None,
        }
    }
}

pub fn parse_duration(raw: &str) -> Result<Duration, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("must not be empty".to_string());
    }
    if let Some(secs) = raw.strip_suffix('s') {
        if secs.is_empty() {
            return Err("expected integer before 's'".to_string());
        }
        let n: u64 = secs
            .parse()
            .map_err(|_| "expected integer before 's'".to_string())?;
        return Ok(Duration::from_secs(n));
    }
    if let Some(mins) = raw.strip_suffix('m') {
        if mins.is_empty() {
            return Err("expected integer before 'm'".to_string());
        }
        let n: u64 = mins
            .parse()
            .map_err(|_| "expected integer before 'm'".to_string())?;
        return Ok(Duration::from_secs(
            n.checked_mul(60)
                .ok_or_else(|| "duration overflow".to_string())?,
        ));
    }
    Err("expected suffix 's' or 'm' (e.g. 15s, 1m)".to_string())
}

pub fn parse_byte_size(raw: &str) -> Result<usize, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("must not be empty".to_string());
    }
    for (suffix, multiplier) in [
        ("k", 1024u64),
        ("m", 1024 * 1024),
        ("g", 1024 * 1024 * 1024),
    ] {
        if let Some(val) = raw.strip_suffix(suffix) {
            if val.is_empty() {
                return Err(format!("expected integer before '{suffix}'"));
            }
            let n: u64 = val
                .parse()
                .map_err(|_| format!("expected integer before '{suffix}'"))?;
            let bytes = n
                .checked_mul(multiplier)
                .ok_or_else(|| "size overflow".to_string())?;
            return usize::try_from(bytes).map_err(|_| "size too large".to_string());
        }
    }
    Err("expected suffix 'k', 'm', or 'g' (e.g. 512k, 2m, 1g)".to_string())
}

fn parse_duration_field(raw: &str) -> Result<Duration, ConfigError> {
    parse_duration(raw).map_err(|message| ConfigError::InvalidDuration {
        value: raw.to_string(),
        message,
    })
}

fn parse_byte_size_field(raw: &str) -> Result<usize, ConfigError> {
    parse_byte_size(raw).map_err(|message| ConfigError::InvalidByteSize {
        value: raw.to_string(),
        message,
    })
}

fn parse_models(name: &str, models: ModelsField) -> Result<(bool, HashSet<String>), ConfigError> {
    match models {
        ModelsField::Single(value) => {
            if value == "default" {
                Ok((true, HashSet::new()))
            } else if value.trim().is_empty() {
                Err(ConfigError::Provider {
                    name: name.to_string(),
                    message: "models must not be empty".to_string(),
                })
            } else {
                Ok((false, HashSet::from([value])))
            }
        }
        ModelsField::List(values) => {
            if values.is_empty() {
                return Err(ConfigError::Provider {
                    name: name.to_string(),
                    message: "models list must not be empty".to_string(),
                });
            }
            if values.iter().any(|model| model == "default") {
                return Err(ConfigError::Provider {
                    name: name.to_string(),
                    message: "use models = \"default\" instead of listing default in an array"
                        .to_string(),
                });
            }
            for model in &values {
                if model.trim().is_empty() {
                    return Err(ConfigError::Provider {
                        name: name.to_string(),
                        message: "model name must not be empty".to_string(),
                    });
                }
            }
            Ok((false, values.into_iter().collect()))
        }
    }
}

/// Parse a diagnostics sink from a user-supplied string.
///
/// The strings "stdout" and "stderr" are reserved for console sinks.
fn sink_from_raw(raw: Option<SinkRaw>) -> Sink {
    match raw {
        Some(SinkRaw::Simple(s)) => match s.trim() {
            "stdout" => Sink::Stdout,
            "stderr" => Sink::Stderr,
            path => Sink::File(path.into()),
        },
        Some(SinkRaw::PerSection { per_section }) => Sink::FilePerSection(per_section.into()),
        None => Sink::Stderr,
    }
}

fn config_path() -> PathBuf {
    if let Ok(path) = env::var("INF_SPLITTER_CONFIG") {
        return PathBuf::from(path);
    }
    PathBuf::from("config/inf-splitter.toml")
}

fn resolve_listen_addr(listen: Option<&str>, port: Option<u16>) -> Result<SocketAddr, ConfigError> {
    let host = listen
        .map(String::from)
        .or_else(|| std::env::var("INF_SPLITTER_LISTEN_HOST").ok())
        .unwrap_or_else(|| "127.0.0.1".into());
    let port = port.unwrap_or(3000);
    format!("{host}:{port}")
        .parse()
        .map_err(|err| ConfigError::Port(format!("{host}:{port}: {err}")))
}

/// Cap a numeric field in a JSON body: if missing, set to limit; if exceeding,
/// clamp down.
pub fn cap_numeric_field(body: &mut serde_json::Value, field: &str, limit: u32) {
    let limit_val = serde_json::json!(limit);
    match body.get(field).and_then(|v| v.as_u64()) {
        Some(existing) if existing > limit as u64 => {
            body[field] = limit_val;
        }
        None => {
            body[field] = limit_val;
        }
        _ => {}
    }
}

pub fn resolve_secret(value: &str) -> Result<String, ConfigError> {
    if let Some(var) = value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
    {
        resolve_var(var)
    } else {
        Ok(value.to_string())
    }
}

fn resolve_var(name: &str) -> Result<String, ConfigError> {
    if let Ok(value) = env::var(name) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let path = Path::new("secrets").join(name);
    if path.is_file() {
        let content = fs::read_to_string(&path)?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(ConfigError::SecretNotFound(name.to_string()));
        }
        return Ok(trimmed.to_string());
    }

    Err(ConfigError::SecretNotFound(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    #[test]
    fn resolve_secret_from_env() {
        let _guard = env_lock();
        env::set_var("TEST_RESOLVE_KEY", "from-env");
        assert_eq!(
            resolve_secret("${TEST_RESOLVE_KEY}").expect("env secret"),
            "from-env"
        );
        env::remove_var("TEST_RESOLVE_KEY");
    }

    #[test]
    fn resolve_secret_literal() {
        assert_eq!(resolve_secret("sk-static").expect("literal"), "sk-static");
    }

    #[test]
    fn parse_duration_accepts_s_and_m_suffixes() {
        assert_eq!(parse_duration("15s").unwrap(), Duration::from_secs(15));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn parse_duration_rejects_invalid_values() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("15").is_err());
        assert!(parse_duration("15x").is_err());
    }

    #[test]
    fn parse_byte_size_accepts_k_and_m_suffixes() {
        assert_eq!(parse_byte_size("512k").unwrap(), 512 * 1024);
        assert_eq!(parse_byte_size("2m").unwrap(), 2 * 1024 * 1024);
    }

    #[test]
    fn parse_byte_size_accepts_g_suffix() {
        assert_eq!(parse_byte_size("1g").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_byte_size("5g").unwrap(), 5 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_byte_size_rejects_invalid_values() {
        assert!(parse_byte_size("").is_err());
        assert!(parse_byte_size("512").is_err());
        assert!(parse_byte_size("512x").is_err());
    }

    #[test]
    fn load_config_with_timeout_and_limits() {
        let _guard = env_lock();
        std::env::remove_var("INF_SPLITTER_LISTEN_HOST");
        let raw = r#"
listen_port =3001
upstream_timeout = "15s"
max_request_body = "512k"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config with limits");
        assert_eq!(config.listen_addr, "127.0.0.1:3001".parse().unwrap());
        assert_eq!(config.upstream_timeout, Duration::from_secs(15));
        assert_eq!(config.max_request_body, 512 * 1024);
    }

    #[test]
    fn listen_defaults_to_localhost() {
        let _guard = env_lock();
        std::env::remove_var("INF_SPLITTER_LISTEN_HOST");
        let raw = r#"
[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert_eq!(config.listen_addr, "127.0.0.1:3000".parse().unwrap());
    }

    #[test]
    fn listen_via_config() {
        let raw = r#"
listen_host = "0.0.0.0"
listen_port =8080

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert_eq!(config.listen_addr, "0.0.0.0:8080".parse().unwrap());
    }

    #[test]
    fn listen_via_env() {
        let _guard = env_lock();
        std::env::set_var("INF_SPLITTER_LISTEN_HOST", "10.0.0.1");
        let raw = r#"
[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert_eq!(config.listen_addr, "10.0.0.1:3000".parse().unwrap());
        std::env::remove_var("INF_SPLITTER_LISTEN_HOST");
    }

    #[test]
    fn listen_config_overrides_env() {
        let _guard = env_lock();
        std::env::set_var("INF_SPLITTER_LISTEN_HOST", "10.0.0.1");
        let raw = r#"
listen_host = "192.168.1.1"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert_eq!(config.listen_addr, "192.168.1.1:3000".parse().unwrap());
        std::env::remove_var("INF_SPLITTER_LISTEN_HOST");
    }

    #[test]
    fn load_config_rejects_invalid_timeout() {
        let raw = r#"
upstream_timeout = "15x"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let err = Config::load_from_str(raw).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidDuration { .. }));
    }

    #[test]
    fn load_config_rejects_no_endpoint() {
        let raw = r#"
[local]
models = "test-model"
"#;
        let err = Config::load_from_str(raw).unwrap_err();
        assert!(matches!(err, ConfigError::Provider { .. }));
    }

    #[test]
    fn load_config_allows_both_endpoints() {
        let raw = r#"
[local]
endpoint_openai = "http://127.0.0.1:11434"
endpoint_anthropic = "https://api.example.com/anthropic"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("both endpoints");
        let route = config.resolve_route("test-model").expect("route");
        assert_eq!(
            route.endpoint_openai.as_deref(),
            Some("http://127.0.0.1:11434")
        );
        assert_eq!(
            route.endpoint_anthropic.as_deref(),
            Some("https://api.example.com/anthropic")
        );
    }

    #[test]
    fn load_project_config_and_resolve_routes() {
        let _guard = env_lock();
        env::set_var("DEEPSEEK_API_KEY", "sk-deepseek-test");
        env::set_var("MAAS_API_KEY", "sk-maas-test");

        let config = Config::load_from_str(include_str!("../config/inf-splitter.toml.example"))
            .expect("example config");
        assert_eq!(config.listen_addr, "127.0.0.1:3000".parse().unwrap());

        let local = config.resolve_route("local-model").expect("local route");
        assert_eq!(
            local.endpoint_anthropic.as_deref(),
            Some("http://127.0.0.1:11345")
        );
        assert!(local.endpoint_openai.is_none());
        assert!(local.api_key.is_none());

        let deepseek = config
            .resolve_route("deepseek-v4-pro")
            .expect("deepseek route");
        assert!(deepseek.endpoint_openai.is_none());
        assert_eq!(
            deepseek.endpoint_anthropic.as_deref(),
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(deepseek.api_key.as_deref(), Some("sk-deepseek-test"));

        let default = config
            .resolve_route("unknown-model")
            .expect("default route");
        assert_eq!(
            default.endpoint_openai.as_deref(),
            Some("https://api.modelarts-maas.com/openai/v1")
        );
        assert!(default.endpoint_anthropic.is_none());
        assert_eq!(default.api_key.as_deref(), Some("sk-maas-test"));

        env::remove_var("DEEPSEEK_API_KEY");
        env::remove_var("MAAS_API_KEY");
    }

    #[test]
    fn global_defaults_merge_with_per_provider_overrides() {
        let raw = r#"
listen_port =3000

[defaults]
max_tokens = 4096
max_completion_tokens = 8192

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "local-model"

[remote]
endpoint_anthropic = "https://api.example.com/anthropic"
models = "remote-model"
max_tokens = 1024
"#;
        let config = Config::load_from_str(raw).expect("config with defaults");

        let local = config.resolve_route("local-model").expect("local");
        assert_eq!(local.max_tokens, Some(4096));
        assert_eq!(local.max_completion_tokens, Some(8192));
        assert_eq!(local.max_output_tokens, None);

        let remote = config.resolve_route("remote-model").expect("remote");
        assert_eq!(remote.max_tokens, Some(1024));
        assert_eq!(remote.max_completion_tokens, Some(8192));
    }

    #[test]
    fn cap_numeric_field_sets_missing() {
        let mut body = serde_json::json!({});
        cap_numeric_field(&mut body, "max_tokens", 1024);
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn cap_numeric_field_clamps_exceeding() {
        let mut body = serde_json::json!({"max_tokens": 4096});
        cap_numeric_field(&mut body, "max_tokens", 1024);
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn cap_numeric_field_leaves_below_unchanged() {
        let mut body = serde_json::json!({"max_tokens": 512});
        cap_numeric_field(&mut body, "max_tokens", 1024);
        assert_eq!(body["max_tokens"], 512);
    }

    #[test]
    fn cap_numeric_field_leaves_equal_unchanged() {
        let mut body = serde_json::json!({"max_tokens": 1024});
        cap_numeric_field(&mut body, "max_tokens", 1024);
        assert_eq!(body["max_tokens"], 1024);
    }

    // --- drop_fields config parsing ---

    #[test]
    fn drop_fields_flat_list_parses() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
drop_fields = ["thinking", "stream_options"]
"#;
        let config = Config::load_from_str(raw).expect("config");
        let route = config.resolve_route("test-model").expect("route");
        match &route.drop_fields {
            DropFields::All(fields) => {
                assert!(fields.contains("thinking"));
                assert!(fields.contains("stream_options"));
                assert_eq!(fields.len(), 2);
            }
            other => panic!("expected All, got {other:?}"),
        }
    }

    #[test]
    fn drop_fields_per_model_parses() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = ["model-a", "model-b"]

[local.drop_fields]
all = ["thinking"]
"model-a" = ["context_management"]
"#;
        let config = Config::load_from_str(raw).expect("config");
        let route = config.resolve_route("model-a").expect("route");
        match &route.drop_fields {
            DropFields::PerModel { all, by_model } => {
                assert!(all.contains("thinking"));
                assert_eq!(all.len(), 1);
                let extra = by_model.get("model-a").expect("model-a entry");
                assert!(extra.contains("context_management"));
                assert_eq!(extra.len(), 1);
            }
            other => panic!("expected PerModel, got {other:?}"),
        }
    }

    #[test]
    fn drop_fields_per_model_all_only() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"

[local.drop_fields]
all = ["thinking"]
"#;
        let config = Config::load_from_str(raw).expect("config");
        let route = config.resolve_route("test-model").expect("route");
        match &route.drop_fields {
            DropFields::PerModel { all, by_model } => {
                assert!(all.contains("thinking"));
                assert!(by_model.is_empty());
            }
            other => panic!("expected PerModel, got {other:?}"),
        }
    }

    #[test]
    fn drop_fields_absent_defaults_to_empty_all() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        let route = config.resolve_route("test-model").expect("route");
        assert!(route.drop_fields.is_empty());
    }

    #[test]
    fn drop_fields_empty_list_is_noop() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
drop_fields = []
"#;
        let config = Config::load_from_str(raw).expect("config");
        let route = config.resolve_route("test-model").expect("route");
        assert!(route.drop_fields.is_empty());
    }

    #[test]
    fn drop_fields_for_model_flat() {
        let fields = DropFields::All(HashSet::from(["a".into(), "b".into()]));
        let result = fields.for_model("any-model");
        assert_eq!(result, HashSet::from(["a".into(), "b".into()]));
    }

    #[test]
    fn drop_fields_for_model_merges_all_and_specific() {
        let fields = DropFields::PerModel {
            all: HashSet::from(["all-field".into()]),
            by_model: HashMap::from([("model-x".into(), HashSet::from(["specific-field".into()]))]),
        };
        let result = fields.for_model("model-x");
        assert!(result.contains("all-field"));
        assert!(result.contains("specific-field"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn drop_fields_for_model_all_only_no_match() {
        let fields = DropFields::PerModel {
            all: HashSet::from(["all-field".into()]),
            by_model: HashMap::new(),
        };
        let result = fields.for_model("model-x");
        assert_eq!(result, HashSet::from(["all-field".into()]));
    }

    // --- config validation ---

    #[test]
    fn rejects_empty_model_name_in_list() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = ["valid", ""]
"#;
        let err = Config::load_from_str(raw).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("model name must not be empty"), "got: {msg}");
    }

    #[test]
    fn rejects_whitespace_model_name_in_list() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = ["valid", "  "]
"#;
        let err = Config::load_from_str(raw).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("model name must not be empty"), "got: {msg}");
    }

    #[test]
    fn rejects_drop_fields_unknown_model() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "known-model"

[local.drop_fields]
"unknown-model" = ["field"]
"#;
        let err = Config::load_from_str(raw).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown-model"), "got: {msg}");
        assert!(msg.contains("drop_fields"), "got: {msg}");
    }

    #[test]
    fn drop_fields_all_key_is_valid_when_no_models_match() {
        // "all" is always valid even when there are no model-specific matches
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "known-model"

[local.drop_fields]
all = ["field"]
"#;
        assert!(Config::load_from_str(raw).is_ok());
    }

    #[test]
    fn rejects_unknown_key_in_defaults_section() {
        let raw = r#"
listen_port =3000

[defaults]
max_tokens = 4096
endpoint_openai = "http://127.0.0.1:11434"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let err = Config::load_from_str(raw).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("endpoint_openai"), "got: {msg}");
    }

    // --- error_translation config parsing ---

    #[test]
    fn error_translation_status_only_parses() {
        let raw = r#"
listen_port =3000

[[error_translation]]
status = 502
egress = "BODY TOO LARGE"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert_eq!(config.error_translation.len(), 1);
        assert_eq!(config.error_translation[0].status, 502);
        assert!(config.error_translation[0].ingress.is_none());
        assert_eq!(config.error_translation[0].egress, "BODY TOO LARGE");
    }

    #[test]
    fn error_translation_status_with_ingress_parses() {
        let raw = r#"
listen_port =3000

[[error_translation]]
status = 413
ingress = "vague message"
egress = "body too large"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert_eq!(config.error_translation.len(), 1);
        assert_eq!(config.error_translation[0].status, 413);
        assert_eq!(
            config.error_translation[0].ingress.as_deref(),
            Some("vague message")
        );
        assert_eq!(config.error_translation[0].egress, "body too large");
    }

    #[test]
    fn error_translation_multiple_rules_parses_in_order() {
        let raw = r#"
listen_port =3000

[[error_translation]]
status = 413
ingress = "vague"
egress = "first"

[[error_translation]]
status = 502
egress = "second"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert_eq!(config.error_translation.len(), 2);
        assert_eq!(config.error_translation[0].egress, "first");
        assert_eq!(config.error_translation[1].egress, "second");
    }

    #[test]
    fn error_translation_absent_is_empty() {
        let raw = r#"
listen_port =3000

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert!(config.error_translation.is_empty());
    }

    #[test]
    fn error_translation_empty_list_is_noop() {
        let raw = r#"
listen_port =3000
error_translation = []

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert!(config.error_translation.is_empty());
    }

    #[test]
    fn rejects_error_translation_missing_status() {
        let raw = r#"
listen_port =3000

[[error_translation]]
egress = "no status field"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let err = Config::load_from_str(raw).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("status"), "got: {msg}");
    }

    #[test]
    fn rejects_error_translation_missing_egress() {
        let raw = r#"
listen_port =3000

[[error_translation]]
status = 502

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let err = Config::load_from_str(raw).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("egress"), "got: {msg}");
    }

    // --- interactions config ---

    #[test]
    fn endpoint_interactions_parses() {
        let raw = r#"
listen_port =3000

[local]
endpoint_interactions = "https://generativelanguage.googleapis.com/v1beta/interactions?model"
models = "gemini-3.1-flash-lite"
"#;
        let config = Config::load_from_str(raw).expect("interactions config");
        let route = config
            .resolve_route("gemini-3.1-flash-lite")
            .expect("route");
        assert!(route.endpoint_interactions.is_some());
        assert!(route.endpoint_openai.is_none());
        assert!(route.endpoint_anthropic.is_none());
    }

    #[test]
    fn endpoint_interactions_with_proxy_and_limits() {
        let raw = r#"
listen_port =3000

[local]
endpoint_interactions = "https://generativelanguage.googleapis.com/v1beta/interactions?model"
models = "gemini-3.1-flash-lite"
proxy = "http://127.0.0.1:8081"
proxy_limit = "130k"
control_clean_all = "***!___!--- очисти все сессии ---!___!***"
control_extend_lifetime = "***!___!--- текущую сессию храни до <unix_utc> ---!___!***"
"#;
        let config = Config::load_from_str(raw).expect("interactions full config");
        let route = config
            .resolve_route("gemini-3.1-flash-lite")
            .expect("route");
        assert_eq!(route.proxy.as_deref(), Some("http://127.0.0.1:8081"));
        assert_eq!(route.proxy_limit, Some(130 * 1024));
        assert!(route.control_clean_all.is_some());
        assert!(route.control_extend_lifetime.is_some());
    }

    #[test]
    fn interactions_session_store_default_none() {
        let raw = r#"
listen_port =3000

[local]
endpoint_interactions = "https://generativelanguage.googleapis.com/v1beta/interactions?model"
models = "gemini-3.1-flash-lite"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert!(config.interactions_session_store.is_none());
    }

    #[test]
    fn interactions_session_store_custom() {
        let raw = r#"
listen_port =3000
interactions_session_store = "/custom/path/sessions.toml"

[local]
endpoint_interactions = "https://generativelanguage.googleapis.com/v1beta/interactions?model"
models = "gemini-3.1-flash-lite"
"#;
        let config = Config::load_from_str(raw).expect("config");
        assert_eq!(
            config.interactions_session_store.as_deref(),
            Some("/custom/path/sessions.toml")
        );
    }
}
