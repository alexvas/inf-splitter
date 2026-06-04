use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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

impl Protocol {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "OPENAI" => Ok(Self::OpenAi),
            "ANTHROPIC" => Ok(Self::Anthropic),
            other => Err(ConfigError::InvalidProtocol(other.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RouteTarget {
    pub section: String,
    pub endpoint: String,
    pub protocol: Protocol,
    pub api_key: Option<String>,
    pub model_names: HashSet<String>,
}

#[derive(Debug, Clone)]
struct ProviderSection {
    name: String,
    endpoint: String,
    protocol: Protocol,
    api_key: Option<String>,
    model_names: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub omit_stream_options: bool,
    sections: HashMap<String, ProviderSection>,
    model_routes: HashMap<String, String>,
    default_section: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    port: Option<u16>,
    #[serde(flatten)]
    providers: HashMap<String, ProviderConfigRaw>,
}

#[derive(Debug, Deserialize)]
struct ProviderConfigRaw {
    endpoint: String,
    protocol: String,
    models: ModelsField,
    api_key: Option<String>,
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
    #[error("invalid protocol {0}: expected OPENAI or ANTHROPIC")]
    InvalidProtocol(String),
    #[error("duplicate model {model} in sections {first} and {second}")]
    DuplicateModel {
        model: String,
        first: String,
        second: String,
    },
    #[error("multiple default provider sections: {first} and {second}")]
    MultipleDefaults { first: String, second: String },
    #[error("no provider section defines models = \"default\" and model {0} is not listed elsewhere")]
    UnroutableModel(String),
    #[error("secret not found for {0}: set env var or secrets/{0} file")]
    SecretNotFound(String),
    #[error("config must define at least one provider section")]
    NoProviders,
    #[error("invalid listen port: {0}")]
    Port(String),
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
        let omit_stream_options = env_truthy("OMIT_STREAM_OPTIONS");

        let mut sections = HashMap::new();
        let mut model_routes = HashMap::new();
        let mut default_section: Option<String> = None;

        for (name, raw_section) in file.providers {
            let endpoint = raw_section.endpoint.trim().trim_end_matches('/').to_string();
            if endpoint.is_empty() {
                return Err(ConfigError::Provider {
                    name,
                    message: "endpoint must not be empty".to_string(),
                });
            }

            let protocol = Protocol::parse(&raw_section.protocol).map_err(|err| {
                ConfigError::Provider {
                    name: name.clone(),
                    message: err.to_string(),
                }
            })?;

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
                    endpoint,
                    protocol,
                    api_key,
                    model_names,
                },
            );
        }

        Ok(Self {
            listen_addr,
            omit_stream_options,
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
            .expect("section name from routes must exist");

        Ok(RouteTarget {
            section: section.name.clone(),
            endpoint: section.endpoint.clone(),
            protocol: section.protocol,
            api_key: section.api_key.clone(),
            model_names: section.model_names.clone(),
        })
    }

    /// Deterministic, lexicographically sorted union of all explicitly configured model ids.
    pub fn sorted_model_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.model_routes.keys().cloned().collect();
        ids.sort();
        ids.dedup();
        ids
    }

    #[cfg(test)]
    pub fn from_model_routes(model_routes: HashMap<String, String>) -> Self {
        Self {
            listen_addr: "0.0.0.0:3000".parse().expect("test listen addr"),
            omit_stream_options: true,
            sections: HashMap::new(),
            model_routes,
            default_section: None,
        }
    }
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
    if let Ok(raw) = env::var("LISTEN_ADDR") {
        return raw
            .parse()
            .map_err(|err| ConfigError::Port(format!("{raw}: {err}")));
    }

    let port = port.unwrap_or(3000);
    format!("0.0.0.0:{port}")
        .parse()
        .map_err(|err| ConfigError::Port(format!("0.0.0.0:{port}: {err}")))
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
        assert_eq!(
            resolve_secret("sk-static").expect("literal"),
            "sk-static"
        );
    }

    #[test]
    fn protocol_parse_accepts_case_insensitive() {
        assert_eq!(Protocol::parse("openai").unwrap(), Protocol::OpenAi);
        assert_eq!(Protocol::parse("ANTHROPIC").unwrap(), Protocol::Anthropic);
    }

    #[test]
    fn load_project_config_and_resolve_routes() {
        let _guard = env_lock();
        env::set_var("DEEPSEEK_API_KEY", "sk-deepseek-test");
        env::set_var("MAAS_API_KEY", "sk-maas-test");

        let config = Config::load().expect("project config");
        assert_eq!(config.listen_addr.port(), 3383);

        let ollama = config.resolve_route("gemma4:31b").expect("ollama route");
        assert_eq!(ollama.endpoint, "http://127.0.0.1:11434");
        assert_eq!(ollama.protocol, Protocol::OpenAi);
        assert!(ollama.api_key.is_none());

        let deepseek = config.resolve_route("deepseek-v4-pro[1m]").expect("deepseek route");
        assert_eq!(deepseek.endpoint, "https://api.deepseek.com/anthropic");
        assert_eq!(deepseek.api_key.as_deref(), Some("sk-deepseek-test"));

        let default = config.resolve_route("unknown-model").expect("default route");
        assert_eq!(default.endpoint, "https://api.modelarts-maas.com/openai/v1");
        assert_eq!(default.api_key.as_deref(), Some("sk-maas-test"));

        // Same model resolves regardless of ingress; conversion happens in handlers.
        let ollama_again = config.resolve_route("gemma4:31b").expect("ollama route again");
        assert_eq!(ollama_again.protocol, Protocol::OpenAi);

        env::remove_var("DEEPSEEK_API_KEY");
        env::remove_var("MAAS_API_KEY");
    }
}
