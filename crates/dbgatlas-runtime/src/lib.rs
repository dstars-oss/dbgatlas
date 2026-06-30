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

    pub fn resolve_tool_paths(&self) -> ResolvedToolPaths {
        resolve_tool_paths(&self.tools)
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
    pub symbol_path: Option<String>,
    #[serde(default)]
    pub ida: IdaRuntimeConfig,
}

impl ToolRuntimeConfig {
    fn validate(&self) -> Result<(), RuntimeConfigError> {
        if let Some(symbol_path) = &self.symbol_path {
            validate_text_value(symbol_path, "tools.symbol_path")?;
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedToolPaths {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dbgeng: Option<ToolLocation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dbgeng_candidates: Vec<ToolLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttd: Option<ToolLocation>,
}

impl ResolvedToolPaths {
    pub fn dbgeng_dir(&self) -> Option<&Path> {
        self.dbgeng.as_ref().map(|location| location.dir.as_path())
    }

    pub fn dbgeng_dirs(&self) -> impl Iterator<Item = &Path> {
        self.dbgeng_candidates
            .iter()
            .map(|location| location.dir.as_path())
    }

    pub fn ttd_dir(&self) -> Option<&Path> {
        self.ttd.as_ref().map(|location| location.dir.as_path())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolLocation {
    pub dir: PathBuf,
    pub source: ToolPathSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPathSource {
    DbgAtlasInstall,
    AppStore,
    WindowsSdk,
    System32,
    DbgEngSibling,
}

pub fn resolve_tool_paths(_tools: &ToolRuntimeConfig) -> ResolvedToolPaths {
    let roots = ToolSearchRoots::default();
    resolve_tool_paths_from_roots(&roots)
}

fn resolve_tool_paths_from_roots(roots: &ToolSearchRoots) -> ResolvedToolPaths {
    let dbgeng_candidates = resolve_dbgeng_dirs_from_roots(roots);
    let dbgeng = dbgeng_candidates.first().cloned();
    let ttd = resolve_ttd_dir_from_roots(&dbgeng_candidates, roots);
    ResolvedToolPaths {
        dbgeng,
        dbgeng_candidates,
        ttd,
    }
}

#[cfg(test)]
fn resolve_dbgeng_dir_from_roots(roots: &ToolSearchRoots) -> Option<ToolLocation> {
    resolve_dbgeng_dirs_from_roots(roots).into_iter().next()
}

fn resolve_dbgeng_dirs_from_roots(roots: &ToolSearchRoots) -> Vec<ToolLocation> {
    // 自动发现只反映当前机器状态，不写进 workspace manifest。
    // 安装态优先使用 DbgAtlas 复制出的 WinDbg runtime，避免从 WindowsApps
    // 直接运行或注入 TTD 组件；缺失时再按 Store -> SDK/WDK -> System32 降级。
    let mut locations = Vec::new();
    if let Some(dir) = find_dbgatlas_windbg_runtime_dir(&roots.dbgatlas_windbg_runtime_dir) {
        push_tool_location(&mut locations, dir, ToolPathSource::DbgAtlasInstall);
    }
    if let Some(dir) = find_store_dbgeng_dir(&roots.program_files_windows_apps) {
        push_tool_location(&mut locations, dir, ToolPathSource::AppStore);
    }
    if let Some(dir) = find_sdk_dbgeng_dir(&roots.windows_kits_roots) {
        push_tool_location(&mut locations, dir, ToolPathSource::WindowsSdk);
    }
    let system32 = roots.system_root.join("System32");
    if find_dbgeng_in_dir(&system32).is_some() {
        push_tool_location(&mut locations, system32, ToolPathSource::System32);
    }
    locations
}

fn push_tool_location(locations: &mut Vec<ToolLocation>, dir: PathBuf, source: ToolPathSource) {
    if locations.iter().any(|location| location.dir == dir) {
        return;
    }
    locations.push(ToolLocation { dir, source });
}

fn resolve_ttd_dir_from_roots(
    dbgeng_candidates: &[ToolLocation],
    roots: &ToolSearchRoots,
) -> Option<ToolLocation> {
    // TTD.exe 通常与对应版本的 dbgeng.dll 配套；先查 dbgeng sibling，
    // 再查独立 Store TimeTravelDebugging 包，避免跨版本组合导致重放失败。
    for dbgeng in dbgeng_candidates {
        if let Some(dir) = find_ttd_in_dir(&dbgeng.dir.join("ttd")) {
            return Some(ToolLocation {
                dir,
                source: ToolPathSource::DbgEngSibling,
            });
        }
    }
    if let Some(dir) = find_store_ttd_dir(&roots.program_files_windows_apps) {
        return Some(ToolLocation {
            dir,
            source: ToolPathSource::AppStore,
        });
    }
    None
}

fn find_dbgeng_in_dir(dir: &Path) -> Option<PathBuf> {
    dir.join("dbgeng.dll").is_file().then(|| dir.to_path_buf())
}

fn find_ttd_in_dir(dir: &Path) -> Option<PathBuf> {
    dir.join("TTD.exe").is_file().then(|| dir.to_path_buf())
}

fn find_dbgatlas_windbg_runtime_dir(dir: &Path) -> Option<PathBuf> {
    find_dbgeng_in_dir(dir).filter(|dbgeng_dir| find_ttd_in_dir(&dbgeng_dir.join("ttd")).is_some())
}

fn find_store_dbgeng_dir(root: &Path) -> Option<PathBuf> {
    let mut packages = store_packages(root, "Microsoft.WinDbg");
    packages.sort();
    packages.reverse();
    packages.into_iter().find_map(|package| {
        find_dbgeng_in_dir(&package.join(windbg_runtime_arch())).or_else(|| {
            find_file_limited(&package, "dbgeng.dll", 4)
                .and_then(|path| path.parent().map(Path::to_path_buf))
        })
    })
}

fn find_store_ttd_dir(root: &Path) -> Option<PathBuf> {
    let mut packages = store_packages(root, "Microsoft.TimeTravelDebugging");
    packages.sort();
    packages.reverse();
    packages
        .into_iter()
        .find_map(|package| find_ttd_in_dir(&package))
}

pub fn default_dbgatlas_windbg_runtime_dir() -> PathBuf {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    program_data
        .join("DbgAtlas")
        .join("rt")
        .join("windbg")
        .join(windbg_runtime_arch())
}

pub fn resolve_store_windbg_dbgeng_dir() -> Option<PathBuf> {
    let roots = ToolSearchRoots::default();
    find_store_dbgeng_dir(&roots.program_files_windows_apps)
}

fn store_packages(root: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix))
        })
        .collect()
}

fn find_sdk_dbgeng_dir(roots: &[PathBuf]) -> Option<PathBuf> {
    let arch = debugger_arch();
    roots.iter().find_map(|root| {
        find_dbgeng_in_dir(&root.join("Debuggers").join(arch)).or_else(|| find_dbgeng_in_dir(root))
    })
}

fn find_file_limited(root: &Path, file_name: &str, max_depth: usize) -> Option<PathBuf> {
    if max_depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
        {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file_limited(&path, file_name, max_depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

fn debugger_arch() -> &'static str {
    if cfg!(target_arch = "x86") {
        "x86"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

fn windbg_runtime_arch() -> &'static str {
    if cfg!(target_arch = "x86") {
        "x86"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    }
}

#[derive(Debug)]
struct ToolSearchRoots {
    dbgatlas_windbg_runtime_dir: PathBuf,
    program_files_windows_apps: PathBuf,
    windows_kits_roots: Vec<PathBuf>,
    system_root: PathBuf,
}

impl Default for ToolSearchRoots {
    fn default() -> Self {
        let program_files = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        let program_files_x86 = std::env::var_os("ProgramFiles(x86)")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut windows_kits_roots = Vec::new();
        for key in [
            "WindowsSdkDir",
            "WDKContentRoot",
            "WindowsSDK_ExecutablePath_x64",
        ] {
            if let Some(path) = std::env::var_os(key).map(PathBuf::from) {
                windows_kits_roots.push(path);
            }
        }
        windows_kits_roots.push(program_files_x86.join("Windows Kits").join("10"));
        windows_kits_roots.push(program_files.join("Windows Kits").join("10"));
        Self {
            dbgatlas_windbg_runtime_dir: default_dbgatlas_windbg_runtime_dir(),
            program_files_windows_apps: program_files.join("WindowsApps"),
            windows_kits_roots,
            system_root,
        }
    }
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
    fn ignores_legacy_local_debug_tool_paths() {
        let config = RuntimeConfig::from_toml_str(
            r#"
version = 1

[tools]
dbgeng_dir = "C:\\stale\\windbg"
ttd_dir = "C:\\stale\\ttd"
symbol_path = "srv*C:\\symbols*https://msdl.microsoft.com/download/symbols"
"#,
        )
        .unwrap();

        assert_eq!(
            config.tools.symbol_path.as_deref(),
            Some("srv*C:\\symbols*https://msdl.microsoft.com/download/symbols")
        );
        let encoded = toml::to_string(&config).unwrap();
        assert!(!encoded.contains("dbgeng_dir"));
        assert!(!encoded.contains("ttd_dir"));
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

    #[test]
    fn resolves_dbgeng_precedence_from_store_sdk_system32() {
        let root = unique_test_dir("runtime-dbgeng-resolver");
        let dbgatlas = root
            .join("DbgAtlas")
            .join("rt")
            .join("windbg")
            .join("amd64");
        let apps = root.join("WindowsApps");
        let sdk = root.join("Windows Kits").join("10");
        let system_root = root.join("Windows");

        touch(dbgatlas.join("dbgeng.dll"));
        touch(dbgatlas.join("ttd").join("TTD.exe"));
        touch(
            apps.join("Microsoft.WinDbg_2.0.0.0_x64__8wekyb3d8bbwe")
                .join("amd64")
                .join("dbgeng.dll"),
        );
        touch(
            sdk.join("Debuggers")
                .join(debugger_arch())
                .join("dbgeng.dll"),
        );
        touch(system_root.join("System32").join("dbgeng.dll"));

        let roots = ToolSearchRoots {
            dbgatlas_windbg_runtime_dir: dbgatlas.clone(),
            program_files_windows_apps: apps.clone(),
            windows_kits_roots: vec![sdk.clone()],
            system_root: system_root.clone(),
        };
        let dbgatlas_location = resolve_dbgeng_dir_from_roots(&roots).unwrap();
        assert_eq!(dbgatlas_location.source, ToolPathSource::DbgAtlasInstall);

        let candidates = resolve_dbgeng_dirs_from_roots(&roots);
        assert_eq!(
            candidates
                .iter()
                .map(|location| location.source)
                .collect::<Vec<_>>(),
            vec![
                ToolPathSource::DbgAtlasInstall,
                ToolPathSource::AppStore,
                ToolPathSource::WindowsSdk,
                ToolPathSource::System32,
            ]
        );

        let roots_without_store = ToolSearchRoots {
            dbgatlas_windbg_runtime_dir: root.join("missing-dbgatlas-runtime"),
            program_files_windows_apps: root.join("missing-apps"),
            windows_kits_roots: vec![sdk.clone()],
            system_root: system_root.clone(),
        };
        let sdk_location = resolve_dbgeng_dir_from_roots(&roots_without_store).unwrap();
        assert_eq!(sdk_location.source, ToolPathSource::WindowsSdk);

        let system32_location = resolve_dbgeng_dir_from_roots(&ToolSearchRoots {
            dbgatlas_windbg_runtime_dir: root.join("missing-dbgatlas-runtime"),
            program_files_windows_apps: root.join("missing-apps"),
            windows_kits_roots: vec![root.join("missing-sdk")],
            system_root,
        })
        .unwrap();
        assert_eq!(system32_location.source, ToolPathSource::System32);
    }

    #[test]
    fn resolves_ttd_precedence_from_dbgeng_sibling_then_store() {
        let root = unique_test_dir("runtime-ttd-resolver");
        let dbgeng = root.join("dbgeng");
        let apps = root.join("WindowsApps");

        touch(dbgeng.join("ttd").join("TTD.exe"));
        touch(
            apps.join("Microsoft.TimeTravelDebugging_2.0.0.0_x64__8wekyb3d8bbwe")
                .join("TTD.exe"),
        );
        let dbgeng_candidates = vec![ToolLocation {
            dir: dbgeng.clone(),
            source: ToolPathSource::AppStore,
        }];

        let roots = ToolSearchRoots {
            dbgatlas_windbg_runtime_dir: root.join("missing-dbgatlas-runtime"),
            program_files_windows_apps: apps.clone(),
            windows_kits_roots: Vec::new(),
            system_root: root.join("Windows"),
        };

        let sibling_location = resolve_ttd_dir_from_roots(&dbgeng_candidates, &roots).unwrap();
        assert_eq!(sibling_location.source, ToolPathSource::DbgEngSibling);
        assert_eq!(sibling_location.dir, dbgeng.join("ttd"));

        let store_location = resolve_ttd_dir_from_roots(&[], &roots).unwrap();
        assert_eq!(store_location.source, ToolPathSource::AppStore);
    }

    #[test]
    fn resolves_dbgatlas_runtime_dbgeng_and_ttd_together() {
        let root = unique_test_dir("runtime-dbgatlas-runtime-resolver");
        let dbgatlas = root
            .join("DbgAtlas")
            .join("rt")
            .join("windbg")
            .join("amd64");
        touch(dbgatlas.join("dbgeng.dll"));
        touch(dbgatlas.join("ttd").join("TTD.exe"));

        let tools = resolve_tool_paths_from_roots(&ToolSearchRoots {
            dbgatlas_windbg_runtime_dir: dbgatlas.clone(),
            program_files_windows_apps: root.join("missing-apps"),
            windows_kits_roots: Vec::new(),
            system_root: root.join("Windows"),
        });

        assert_eq!(
            tools.dbgeng.unwrap().source,
            ToolPathSource::DbgAtlasInstall
        );
        assert_eq!(
            tools.ttd.unwrap(),
            ToolLocation {
                dir: dbgatlas.join("ttd"),
                source: ToolPathSource::DbgEngSibling,
            }
        );
    }

    #[test]
    fn skips_incomplete_dbgatlas_runtime_candidate() {
        let root = unique_test_dir("runtime-dbgatlas-incomplete-resolver");
        let dbgatlas = root
            .join("DbgAtlas")
            .join("rt")
            .join("windbg")
            .join("amd64");
        let apps = root.join("WindowsApps");
        touch(dbgatlas.join("dbgeng.dll"));
        touch(
            apps.join("Microsoft.WinDbg_2.0.0.0_x64__8wekyb3d8bbwe")
                .join("amd64")
                .join("dbgeng.dll"),
        );

        let tools = resolve_tool_paths_from_roots(&ToolSearchRoots {
            dbgatlas_windbg_runtime_dir: dbgatlas,
            program_files_windows_apps: apps,
            windows_kits_roots: Vec::new(),
            system_root: root.join("Windows"),
        });

        assert_eq!(tools.dbgeng.unwrap().source, ToolPathSource::AppStore);
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create test root");
        root
    }

    fn touch(path: PathBuf) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(path, b"").expect("touch file");
    }
}
