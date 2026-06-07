use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde::Deserialize;
use thiserror::Error;

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

#[derive(Debug, Clone)]
pub struct RouteTarget {
    pub section: String,
    pub endpoint_openai: Option<String>,
    pub endpoint_anthropic: Option<String>,
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub max_completion_tokens: Option<u32>,
    pub model_names: HashSet<String>,
}

#[derive(Debug, Clone)]
struct ProviderSection {
    name: String,
    endpoint_openai: Option<String>,
    endpoint_anthropic: Option<String>,
    api_key: Option<String>,
    max_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    max_completion_tokens: Option<u32>,
    model_names: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub upstream_timeout: Duration,
    pub max_request_body: usize,
    pub body_too_large_hint_statuses: Arc<HashSet<StatusCode>>,
    pub dump_on_error: bool,
    default_max_tokens: Option<u32>,
    default_max_output_tokens: Option<u32>,
    default_max_completion_tokens: Option<u32>,
    sections: HashMap<String, ProviderSection>,
    model_routes: HashMap<String, String>,
    default_section: Option<String>,
}

fn default_body_too_large_hint_statuses() -> HashSet<StatusCode> {
    HashSet::from([StatusCode::PAYLOAD_TOO_LARGE])
}

const DEFAULT_UPSTREAM_TIMEOUT: &str = "5m";
const DEFAULT_MAX_REQUEST_BODY: &str = "2m";

#[derive(Debug, Deserialize)]
struct FileConfig {
    port: Option<u16>,
    upstream_timeout: Option<String>,
    max_request_body: Option<String>,
    defaults: Option<DefaultConfig>,
    body_too_large_hint_statuses: Option<Vec<u16>>,
    #[serde(flatten)]
    providers: HashMap<String, ProviderConfigRaw>,
}

#[derive(Debug, Deserialize)]
struct DefaultConfig {
    max_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    max_completion_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ProviderConfigRaw {
    endpoint_openai: Option<String>,
    endpoint_anthropic: Option<String>,
    models: ModelsField,
    api_key: Option<String>,
    max_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    max_completion_tokens: Option<u32>,
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

        let listen_addr = resolve_listen_addr(file.port)?;
        let dump_on_error = env_truthy("DUMP_ON_ERROR");
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

        let body_too_large_hint_statuses = Arc::new(match file.body_too_large_hint_statuses {
            Some(codes) if !codes.is_empty() => codes
                .into_iter()
                .map(|c| StatusCode::from_u16(c).unwrap_or(StatusCode::PAYLOAD_TOO_LARGE))
                .collect(),
            _ => default_body_too_large_hint_statuses(),
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

            if endpoint_openai.is_none() && endpoint_anthropic.is_none() {
                return Err(ConfigError::Provider {
                    name,
                    message: "at least one of endpoint_openai or endpoint_anthropic must be set"
                        .to_string(),
                });
            }

            let api_key = match raw_section.api_key {
                Some(value) => Some(resolve_secret(&value)?),
                None => None,
            };

            let (is_default, model_names) = parse_models(&name, raw_section.models)?;

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
                    api_key,
                    max_tokens: raw_section.max_tokens,
                    max_output_tokens: raw_section.max_output_tokens,
                    max_completion_tokens: raw_section.max_completion_tokens,
                    model_names,
                },
            );
        }

        Ok(Self {
            listen_addr,
            dump_on_error,
            upstream_timeout,
            max_request_body,
            body_too_large_hint_statuses,
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
            api_key: section.api_key.clone(),
            max_tokens: section.max_tokens.or(self.default_max_tokens),
            max_output_tokens: section.max_output_tokens.or(self.default_max_output_tokens),
            max_completion_tokens: section
                .max_completion_tokens
                .or(self.default_max_completion_tokens),
            model_names: section.model_names.clone(),
        })
    }

    /// Deterministic, lexicographically sorted union of all explicitly configured model ids.
    pub fn sorted_model_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.model_routes.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Unique upstream endpoints for health checks.
    pub fn upstream_endpoints(&self) -> Vec<(String, String)> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for section in self.sections.values() {
            let eps = [
                section.endpoint_openai.as_deref(),
                section.endpoint_anthropic.as_deref(),
            ];
            for ep in eps.into_iter().flatten() {
                if seen.insert(ep.to_string()) {
                    result.push((section.name.clone(), ep.to_string()));
                }
            }
        }
        result
    }

    #[cfg(test)]
    pub fn from_model_routes(model_routes: HashMap<String, String>) -> Self {
        Self {
            listen_addr: "0.0.0.0:3000".parse().expect("test listen addr"),
            dump_on_error: false,
            upstream_timeout: parse_duration(DEFAULT_UPSTREAM_TIMEOUT).expect("default timeout"),
            max_request_body: parse_byte_size(DEFAULT_MAX_REQUEST_BODY)
                .expect("default body limit"),
            body_too_large_hint_statuses: Arc::new(default_body_too_large_hint_statuses()),
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
    if let Some(k) = raw.strip_suffix('k') {
        if k.is_empty() {
            return Err("expected integer before 'k'".to_string());
        }
        let n: u64 = k
            .parse()
            .map_err(|_| "expected integer before 'k'".to_string())?;
        let bytes = n
            .checked_mul(1024)
            .ok_or_else(|| "size overflow".to_string())?;
        return usize::try_from(bytes).map_err(|_| "size too large".to_string());
    }
    if let Some(m) = raw.strip_suffix('m') {
        if m.is_empty() {
            return Err("expected integer before 'm'".to_string());
        }
        let n: u64 = m
            .parse()
            .map_err(|_| "expected integer before 'm'".to_string())?;
        let bytes = n
            .checked_mul(1024 * 1024)
            .ok_or_else(|| "size overflow".to_string())?;
        return usize::try_from(bytes).map_err(|_| "size too large".to_string());
    }
    Err("expected suffix 'k' or 'm' (e.g. 512k, 2m)".to_string())
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
            Ok((false, values.into_iter().collect()))
        }
    }
}

fn config_path() -> PathBuf {
    if let Ok(path) = env::var("INF_SPLITTER_CONFIG") {
        return PathBuf::from(path);
    }
    PathBuf::from("config/inf-splitter.toml")
}

fn resolve_listen_addr(port: Option<u16>) -> Result<SocketAddr, ConfigError> {
    let port = port.unwrap_or(3000);
    format!("0.0.0.0:{port}")
        .parse()
        .map_err(|err| ConfigError::Port(format!("0.0.0.0:{port}: {err}")))
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

fn env_truthy(name: &str) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
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
    fn parse_byte_size_rejects_invalid_values() {
        assert!(parse_byte_size("").is_err());
        assert!(parse_byte_size("512").is_err());
        assert!(parse_byte_size("512x").is_err());
    }

    #[test]
    fn load_config_with_timeout_and_limits() {
        let raw = r#"
port = 3001
upstream_timeout = "15s"
max_request_body = "512k"

[local]
endpoint_openai = "http://127.0.0.1:11434"
models = "test-model"
"#;
        let config = Config::load_from_str(raw).expect("config with limits");
        assert_eq!(config.listen_addr, "0.0.0.0:3001".parse().unwrap());
        assert_eq!(config.upstream_timeout, Duration::from_secs(15));
        assert_eq!(config.max_request_body, 512 * 1024);
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

        let config = Config::load().expect("project config");
        assert_eq!(config.listen_addr, "0.0.0.0:3000".parse().unwrap());

        let ollama = config.resolve_route("gemma4:31b").expect("ollama route");
        assert_eq!(
            ollama.endpoint_openai.as_deref(),
            Some("http://host.docker.internal:11434")
        );
        assert!(ollama.endpoint_anthropic.is_none());
        assert!(ollama.api_key.is_none());

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
port = 3000

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
}
