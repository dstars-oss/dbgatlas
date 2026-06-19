use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const RUNTIME_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum RuntimeConfigError {
    #[error("unsupported runtime config version {actual}; expected {expected}")]
    UnsupportedVersion { actual: u32, expected: u32 },
    #[error("server bind address must be loopback: {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("{field} must not be empty")]
    EmptyValue { field: &'static str },
    #[error("{field} contains unsupported control characters")]
    ControlCharacters { field: &'static str },
    #[error("proxy.env key is not supported: {0}")]
    UnsupportedProxyKey(String),
    #[error("parse runtime config: {0}")]
    ParseToml(#[from] toml::de::Error),
    #[error("read runtime config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub version: u32,
    #[serde(default)]
    pub server: ServerRuntimeConfig,
    #[serde(default)]
    pub tools: ToolRuntimeConfig,
    #[serde(default)]
    pub process: ProcessLaunchPolicy,
    #[serde(default)]
    pub proxy: ProxyConfig,
}

impl RuntimeConfig {
    pub fn from_toml_str(input: &str) -> Result<Self, RuntimeConfigError> {
        let config = toml::from_str::<Self>(input)?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, RuntimeConfigError> {
        let path = path.as_ref();
        let input = std::fs::read_to_string(path).map_err(|source| RuntimeConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml_str(&input)
    }

    pub fn validate(&self) -> Result<(), RuntimeConfigError> {
        if self.version != RUNTIME_CONFIG_VERSION {
            return Err(RuntimeConfigError::UnsupportedVersion {
                actual: self.version,
                expected: RUNTIME_CONFIG_VERSION,
            });
        }
        if !self.server.bind.ip().is_loopback() {
            return Err(RuntimeConfigError::NonLoopbackBind(self.server.bind));
        }
        self.tools.validate()?;
        self.proxy.validate()?;
        Ok(())
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            version: RUNTIME_CONFIG_VERSION,
            server: ServerRuntimeConfig::default(),
            tools: ToolRuntimeConfig::default(),
            process: ProcessLaunchPolicy::default(),
            proxy: ProxyConfig::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRuntimeConfig {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
}

impl Default for ServerRuntimeConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRuntimeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dbgeng_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttd_dir: Option<PathBuf>,
    #[serde(default)]
    pub ida: IdaRuntimeConfig,
}

impl ToolRuntimeConfig {
    fn validate(&self) -> Result<(), RuntimeConfigError> {
        if let Some(path) = &self.dbgeng_dir {
            validate_path_value(path, "tools.dbgeng_dir")?;
        }
        if let Some(symbol_path) = &self.symbol_path {
            validate_text_value(symbol_path, "tools.symbol_path")?;
        }
        if let Some(path) = &self.ttd_dir {
            validate_path_value(path, "tools.ttd_dir")?;
        }
        self.ida.validate()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdaRuntimeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_src_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_py_eval: bool,
}

impl IdaRuntimeConfig {
    fn validate(&self) -> Result<(), RuntimeConfigError> {
        if let Some(path) = &self.install_dir {
            validate_path_value(path, "tools.ida.install_dir")?;
        }
        if let Some(path) = &self.python_executable {
            validate_path_value(path, "tools.ida.python_executable")?;
        }
        if let Some(path) = &self.vendor_src_dir {
            validate_path_value(path, "tools.ida.vendor_src_dir")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessLaunchPolicy {
    #[serde(default)]
    pub child_identity: ChildIdentity,
    #[serde(default)]
    pub fallback_child_identity: FallbackChildIdentity,
    #[serde(default)]
    pub elevate_if_admin: bool,
}

impl Default for ProcessLaunchPolicy {
    fn default() -> Self {
        Self {
            child_identity: ChildIdentity::CurrentProcess,
            fallback_child_identity: FallbackChildIdentity::CurrentProcess,
            elevate_if_admin: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildIdentity {
    #[default]
    CurrentProcess,
    McpPeerSession,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackChildIdentity {
    #[default]
    CurrentProcess,
    ActiveInteractiveSession,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProxyConfig {
    #[default]
    None,
    Disabled,
    Url {
        url: String,
    },
    Env {
        variables: BTreeMap<String, String>,
    },
}

impl ProxyConfig {
    fn validate(&self) -> Result<(), RuntimeConfigError> {
        match self {
            Self::None | Self::Disabled => Ok(()),
            Self::Url { url } => validate_text_value(url, "proxy.url"),
            Self::Env { variables } => {
                for (key, value) in variables {
                    if !KNOWN_PROXY_KEYS.contains(&key.as_str()) {
                        return Err(RuntimeConfigError::UnsupportedProxyKey(key.clone()));
                    }
                    validate_text_value(value, "proxy.env")?;
                }
                Ok(())
            }
        }
    }
}

const KNOWN_PROXY_KEYS: &[&str] = &[
    "_NT_SYMBOL_PROXY",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

fn default_bind() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

fn validate_path_value(path: &Path, field: &'static str) -> Result<(), RuntimeConfigError> {
    if path.as_os_str().is_empty() {
        return Err(RuntimeConfigError::EmptyValue { field });
    }
    validate_text_value(&path.as_os_str().to_string_lossy(), field)
}

fn validate_text_value(value: &str, field: &'static str) -> Result<(), RuntimeConfigError> {
    if value.trim().is_empty() {
        return Err(RuntimeConfigError::EmptyValue { field });
    }
    if value
        .chars()
        .any(|ch| matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}') || ch.is_control())
    {
        return Err(RuntimeConfigError::ControlCharacters { field });
    }
    Ok(())
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_runtime_config_without_workspace_manifest_fields() {
        let config = RuntimeConfig::from_toml_str(
            r#"
version = 1

[server]
bind = "127.0.0.1:7331"

[tools]
symbol_path = "srv*C:\\symbols*https://msdl.microsoft.com/download/symbols"

[proxy]
mode = "none"
"#,
        )
        .unwrap();

        assert_eq!(config.server.bind.port(), 7331);
        assert_eq!(
            config.tools.symbol_path.as_deref(),
            Some("srv*C:\\symbols*https://msdl.microsoft.com/download/symbols")
        );
        assert!(!config.tools.ida.allow_py_eval);
        let encoded = toml::to_string(&config).unwrap();
        assert!(!encoded.contains("workspace_id"));
        assert!(!encoded.contains("dbgatlas-workspace"));
    }

    #[test]
    fn parses_ida_py_eval_opt_in() {
        let config = RuntimeConfig::from_toml_str(
            r#"
version = 1

[tools.ida]
allow_py_eval = true
"#,
        )
        .unwrap();

        assert!(config.tools.ida.allow_py_eval);
        let encoded = toml::to_string(&config).unwrap();
        assert!(encoded.contains("allow_py_eval = true"));
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let error = RuntimeConfig::from_toml_str(
            r#"
version = 1

[server]
bind = "0.0.0.0:7331"
"#,
        )
        .unwrap_err();

        assert!(matches!(error, RuntimeConfigError::NonLoopbackBind(_)));
    }

    #[test]
    fn rejects_symbol_path_control_characters() {
        let error = RuntimeConfig::from_toml_str(
            r#"
version = 1

[tools]
symbol_path = "srv*C:\\symbols\n.shell dir"
"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            RuntimeConfigError::ControlCharacters {
                field: "tools.symbol_path"
            }
        ));
    }

    #[test]
    fn validates_proxy_keys() {
        let error = RuntimeConfig::from_toml_str(
            r#"
version = 1

[proxy]
mode = "env"

[proxy.variables]
BAD_PROXY = "http://127.0.0.1:7897"
"#,
        )
        .unwrap_err();

        assert!(matches!(error, RuntimeConfigError::UnsupportedProxyKey(_)));
    }
}
