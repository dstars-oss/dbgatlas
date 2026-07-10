use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceInstallPaths {
    pub root_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub staging_bin_dir: PathBuf,
    pub etc_dir: PathBuf,
    pub rt_dir: PathBuf,
    pub windbg_runtime_dir: PathBuf,
    pub var_dir: PathBuf,
    pub log_dir: PathBuf,
    pub config_path: PathBuf,
    pub token_file: PathBuf,
    pub installed_exe: PathBuf,
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn service_log_pruning_keeps_seven_calendar_days() {
        let temp = tempfile::tempdir().unwrap();
        let current_day =
            windows_service_control::unix_day_from_date(2026, 6, 18).expect("valid date");
        for offset in 0..8 {
            let day = current_day - offset;
            let date = windows_service_control::utc_date_from_unix_day(day);
            fs::write(temp.path().join(format!("service-{date}.log")), "").unwrap();
        }
        fs::write(temp.path().join("other.log"), "").unwrap();

        windows_service_control::prune_service_logs(temp.path(), current_day).unwrap();

        assert!(temp.path().join("service-2026-06-18.log").is_file());
        assert!(temp.path().join("service-2026-06-12.log").is_file());
        assert!(!temp.path().join("service-2026-06-11.log").exists());
        assert!(temp.path().join("other.log").is_file());
    }

    #[test]
    fn service_log_file_day_rejects_invalid_dates() {
        assert!(windows_service_control::service_log_file_day("service-2026-02-29.log").is_none());
        assert!(windows_service_control::service_log_file_day("service-2024-02-29.log").is_some());
        assert!(windows_service_control::service_log_file_day("service-latest.log").is_none());
    }
}

impl ServiceInstallPaths {
    pub fn for_root(root_dir: PathBuf) -> Self {
        let bin_dir = root_dir.join(WINDOWS_SERVICE_BIN_DIR);
        let etc_dir = root_dir.join(WINDOWS_SERVICE_ETC_DIR);
        let rt_dir = bin_dir.join(WINDOWS_SERVICE_RT_DIR);
        let var_dir = root_dir.join(WINDOWS_SERVICE_VAR_DIR);
        let log_dir = var_dir.join(WINDOWS_SERVICE_LOG_DIR);
        Self {
            staging_bin_dir: root_dir.join("bin.staging"),
            config_path: etc_dir.join(WINDOWS_SERVICE_CONFIG_FILE),
            token_file: etc_dir.join(WINDOWS_SERVICE_TOKEN_FILE),
            installed_exe: bin_dir.join("dbgatlas.exe"),
            windbg_runtime_dir: rt_dir.join("windbg").join(windbg_runtime_arch()),
            bin_dir,
            etc_dir,
            rt_dir,
            var_dir,
            log_dir,
            root_dir,
        }
    }

    pub fn legacy_config_path(&self) -> PathBuf {
        self.root_dir.join(WINDOWS_SERVICE_CONFIG_FILE)
    }

    pub fn legacy_token_file(&self) -> PathBuf {
        self.root_dir.join(WINDOWS_SERVICE_TOKEN_FILE)
    }

    pub fn legacy_rt_dir(&self) -> PathBuf {
        self.root_dir.join(WINDOWS_SERVICE_RT_DIR)
    }

    pub fn legacy_windbg_runtime_dir(&self) -> PathBuf {
        self.legacy_rt_dir()
            .join("windbg")
            .join(windbg_runtime_arch())
    }

    pub fn install_marker_path(&self) -> PathBuf {
        self.root_dir.join(WINDOWS_SERVICE_INSTALL_MARKER_FILE)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServicePayloadFile {
    pub file_name: String,
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowsServicePayloadMode {
    #[default]
    Copy,
    UseExisting,
}

#[derive(Clone, Debug)]
pub struct WindowsServiceInstallOptions {
    pub install_root: Option<PathBuf>,
    pub payload_dir: Option<PathBuf>,
    pub payload_mode: WindowsServicePayloadMode,
    pub bind: SocketAddr,
    pub force: bool,
}

impl Default for WindowsServiceInstallOptions {
    fn default() -> Self {
        Self {
            install_root: None,
            payload_dir: None,
            payload_mode: WindowsServicePayloadMode::Copy,
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_SERVICE_PORT),
            force: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WindowsServiceControlOptions {
    pub install_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct WindowsServiceUninstallOptions {
    pub install_root: Option<PathBuf>,
    pub purge: bool,
}

#[derive(Clone, Debug)]
pub struct WindowsServiceUpdateOptions {
    pub source_dir: PathBuf,
    pub restart: bool,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub struct WindowsServiceApplyUpdateOptions {
    pub install_root: Option<PathBuf>,
    pub source_dir: PathBuf,
    pub restart: bool,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub struct WindowsServiceRunOptions {
    pub install_root: PathBuf,
    pub config_path: PathBuf,
    pub token_file: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowsServiceCommandResult {
    pub service_name: String,
    pub display_name: String,
    pub status: String,
    pub endpoint: Option<SocketAddr>,
    pub installed_binary: PathBuf,
    pub config_path: PathBuf,
    pub token_file: PathBuf,
    pub log_dir: PathBuf,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub payload: Vec<ServicePayloadFile>,
}

#[derive(Clone, Debug, Default)]
pub struct WindowsServiceDoctorOptions {
    pub install_root: Option<PathBuf>,
    pub report_path: Option<PathBuf>,
    pub msi_summary_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCheckStatus {
    Ok,
    Warning,
}

impl DoctorCheckStatus {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub id: String,
    pub label: String,
    pub status: DoctorCheckStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowsServiceDoctorReport {
    pub status: DoctorCheckStatus,
    pub install_root: PathBuf,
    pub checks: Vec<DoctorCheck>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msi_summary_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowsServiceUpdateAccepted {
    pub status: String,
    pub source_dir: PathBuf,
    pub service_name: String,
    pub installed_binary: PathBuf,
    pub log_dir: PathBuf,
    pub payload: Vec<ServicePayloadFile>,
    pub restart: bool,
}

#[derive(Debug)]
pub(super) struct PreparedServiceUpdate {
    pub(super) source_dir: PathBuf,
    pub(super) bin_payload: Vec<ServicePayloadFile>,
    pub(super) config_payload: Option<ServicePayloadFile>,
    pub(super) response: WindowsServiceUpdateAccepted,
}

pub fn default_windows_service_paths() -> ServiceInstallPaths {
    let root = process_service_install_root().unwrap_or_else(default_windows_service_root);
    ServiceInstallPaths::for_root(root)
}

pub fn default_windows_service_root() -> PathBuf {
    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    program_files.join("DbgAtlas")
}

pub(super) fn process_service_install_root() -> Option<PathBuf> {
    SERVICE_INSTALL_ROOT_OVERRIDE.get().cloned()
}

pub(super) fn set_process_service_install_root(root: PathBuf) -> Result<(), ServiceError> {
    if let Some(existing) = SERVICE_INSTALL_ROOT_OVERRIDE.get() {
        if existing == &root {
            return Ok(());
        }
        return Err(ServiceError::ServiceControl(format!(
            "service install root was already set to {}; refusing to reset to {}",
            existing.display(),
            root.display()
        )));
    }
    SERVICE_INSTALL_ROOT_OVERRIDE
        .set(root)
        .map_err(|_| ServiceError::ServiceControl("failed to set service install root".to_string()))
}

pub(super) fn windows_service_paths_for_root(
    install_root: Option<&Path>,
) -> Result<ServiceInstallPaths, ServiceError> {
    if let Some(root) = install_root {
        return Ok(ServiceInstallPaths::for_root(normalize_install_root(root)?));
    }
    if let Some(root) = process_service_install_root() {
        return Ok(ServiceInstallPaths::for_root(root));
    }
    if let Some(paths) = installed_service_paths_from_scm() {
        return Ok(paths);
    }
    Ok(ServiceInstallPaths::for_root(default_windows_service_root()))
}

pub(super) fn windows_service_paths_for_install(
    install_root: Option<&Path>,
) -> Result<ServiceInstallPaths, ServiceError> {
    let root = match install_root {
        Some(root) => normalize_install_root(root)?,
        None => default_windows_service_paths().root_dir,
    };
    Ok(ServiceInstallPaths::for_root(root))
}

pub(super) fn service_paths_from_executable_path(
    executable_path: &Path,
) -> Result<ServiceInstallPaths, ServiceError> {
    let bin_dir = executable_path.parent().ok_or_else(|| {
        ServiceError::ServiceControl(format!(
            "installed service executable has no parent: {}",
            executable_path.display()
        ))
    })?;
    let root_dir = bin_dir.parent().ok_or_else(|| {
        ServiceError::ServiceControl(format!(
            "installed service bin directory has no parent: {}",
            bin_dir.display()
        ))
    })?;
    Ok(ServiceInstallPaths::for_root(root_dir.to_path_buf()))
}

pub(super) fn service_paths_from_scm_binary_path(
    binary_path: &Path,
) -> Result<ServiceInstallPaths, ServiceError> {
    let command_line = binary_path.as_os_str().to_string_lossy();
    let args = split_windows_command_line(&command_line);
    if let Some(root) = service_install_root_from_args(&args) {
        return Ok(ServiceInstallPaths::for_root(normalize_install_root(
            &root,
        )?));
    }
    if let Some(executable) = args.first().filter(|arg| !arg.is_empty()) {
        return service_paths_from_executable_path(Path::new(executable));
    }
    service_paths_from_executable_path(binary_path)
}

pub(super) fn service_install_root_from_args(args: &[String]) -> Option<PathBuf> {
    for (index, arg) in args.iter().enumerate() {
        if arg == "--install-root" {
            return args.get(index + 1).map(PathBuf::from);
        }
        if let Some(value) = arg.strip_prefix("--install-root=") {
            return Some(PathBuf::from(value));
        }
    }
    for (index, arg) in args.iter().enumerate() {
        let config_path = if arg == "--config" {
            args.get(index + 1).map(PathBuf::from)
        } else {
            arg.strip_prefix("--config=").map(PathBuf::from)
        };
        if let Some(config_path) = config_path {
            if let Some(root) = install_root_from_config_path(&config_path) {
                return Some(root);
            }
        }
    }
    None
}

pub(super) fn install_root_from_config_path(config_path: &Path) -> Option<PathBuf> {
    config_path.parent()?.parent().map(PathBuf::from)
}

pub(super) fn split_windows_command_line(command_line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut in_arg = false;
    let mut backslashes = 0usize;

    for ch in command_line.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                in_arg = true;
            }
            '"' => {
                current.extend(std::iter::repeat_n('\\', backslashes / 2));
                if backslashes % 2 == 0 {
                    in_quotes = !in_quotes;
                    in_arg = true;
                } else {
                    current.push('"');
                    in_arg = true;
                }
                backslashes = 0;
            }
            ch if ch.is_whitespace() && !in_quotes => {
                current.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            ch => {
                current.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                current.push(ch);
                in_arg = true;
            }
        }
    }

    current.extend(std::iter::repeat_n('\\', backslashes));
    if in_arg {
        args.push(current);
    }
    args
}

#[cfg(windows)]
pub(super) fn installed_service_paths_from_scm() -> Option<ServiceInstallPaths> {
    let manager = windows_service::service_manager::ServiceManager::local_computer(
        None::<&str>,
        windows_service::service_manager::ServiceManagerAccess::CONNECT,
    )
    .ok()?;
    let service = manager
        .open_service(
            WINDOWS_SERVICE_NAME,
            windows_service::service::ServiceAccess::QUERY_CONFIG,
        )
        .ok()?;
    let config = service.query_config().ok()?;
    service_paths_from_scm_binary_path(&config.executable_path).ok()
}

#[cfg(not(windows))]
pub(super) fn installed_service_paths_from_scm() -> Option<ServiceInstallPaths> {
    None
}

pub(super) fn normalize_install_root(root: &Path) -> Result<PathBuf, ServiceError> {
    if root.as_os_str().is_empty() {
        return Err(ServiceError::ServiceControl(
            "install root must not be empty".to_string(),
        ));
    }
    if root.is_absolute() {
        Ok(root.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(root))
    }
}

pub(super) fn legacy_program_data_service_paths() -> ServiceInstallPaths {
    let root = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("DbgAtlas");
    ServiceInstallPaths::for_root(root)
}

pub(super) fn windbg_runtime_arch() -> &'static str {
    if cfg!(target_arch = "x86") {
        "x86"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    }
}

pub fn discover_service_payload(
    source_dir: &Path,
    destination_dir: &Path,
) -> Result<Vec<ServicePayloadFile>, ServiceError> {
    let mut payload = Vec::new();
    let mut missing = Vec::new();
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        let source = source_dir.join(file_name);
        if !source.is_file() {
            missing.push(source);
            continue;
        }
        payload.push(ServicePayloadFile {
            file_name: (*file_name).to_string(),
            destination: destination_dir.join(file_name),
            source,
        });
    }
    if !missing.is_empty() {
        let files = missing
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ServiceError::IncompleteInstallPayload(format!(
            "missing {files}; build or assemble the complete release payload before installing or updating the service"
        )));
    }
    Ok(payload)
}

pub fn installed_client_config() -> Result<Option<ServiceConfig>, ServiceError> {
    let paths = windows_service_paths_for_root(None)?;
    installed_client_config_from_paths(&paths)
}

pub(super) fn installed_client_config_from_paths(
    paths: &ServiceInstallPaths,
) -> Result<Option<ServiceConfig>, ServiceError> {
    if !paths.installed_exe.is_file() || !paths.config_path.is_file() || !paths.token_file.is_file()
    {
        return Ok(None);
    }
    let runtime = RuntimeConfig::load(&paths.config_path)?;
    let bearer_token = fs::read_to_string(&paths.token_file)?.trim().to_string();
    let config = ServiceConfig {
        bind: runtime.server.bind,
        bearer_token,
    };
    validate_config(&config)?;
    Ok(Some(config))
}

pub fn run_windows_service_doctor(
    options: WindowsServiceDoctorOptions,
) -> Result<WindowsServiceDoctorReport, ServiceError> {
    let paths = windows_service_paths_for_root(options.install_root.as_deref())?;
    let (runtime, config_check) = load_doctor_runtime_config(&paths);
    let tools =
        runtime.resolve_tool_paths_with_dbgatlas_windbg_runtime_dir(&paths.windbg_runtime_dir);
    let report = collect_windows_service_doctor_report(
        &paths,
        &runtime,
        &tools,
        config_check,
        options.report_path,
        options.msi_summary_path,
    );
    if let Some(report_path) = &report.report_path {
        write_text_file(report_path, &format_doctor_report(&report))?;
    }
    if let Some(summary_path) = &report.msi_summary_path {
        write_text_file(summary_path, &format_doctor_msi_summary(&report))?;
    }
    let report_path_for_log = report
        .report_path
        .as_ref()
        .map(|path| path.display().to_string());
    append_service_diagnostic_log_for_paths(
        &paths,
        &format!(
            "service_doctor status={} warning_count={} report_path={}",
            report.status.label(),
            report.warnings.len(),
            log_value(report_path_for_log.as_deref())
        ),
    );
    Ok(report)
}

pub(super) fn load_doctor_runtime_config(
    paths: &ServiceInstallPaths,
) -> (RuntimeConfig, DoctorCheck) {
    if !paths.config_path.is_file() {
        return (
            RuntimeConfig::default(),
            doctor_check(
                "runtime_config",
                "Runtime config",
                DoctorCheckStatus::Warning,
                format!(
                    "runtime config was not found at {}; using defaults for diagnostics",
                    paths.config_path.display()
                ),
                None,
                Some(paths.config_path.clone()),
            ),
        );
    }
    match RuntimeConfig::load(&paths.config_path) {
        Ok(runtime) => (
            runtime,
            doctor_check(
                "runtime_config",
                "Runtime config",
                DoctorCheckStatus::Ok,
                "runtime config loaded".to_string(),
                None,
                Some(paths.config_path.clone()),
            ),
        ),
        Err(error) => (
            RuntimeConfig::default(),
            doctor_check(
                "runtime_config",
                "Runtime config",
                DoctorCheckStatus::Warning,
                format!(
                    "runtime config could not be loaded; using defaults for diagnostics: {error}"
                ),
                None,
                Some(paths.config_path.clone()),
            ),
        ),
    }
}

pub(super) fn collect_windows_service_doctor_report(
    paths: &ServiceInstallPaths,
    runtime: &RuntimeConfig,
    tools: &dbgatlas_runtime::ResolvedToolPaths,
    config_check: DoctorCheck,
    report_path: Option<PathBuf>,
    msi_summary_path: Option<PathBuf>,
) -> WindowsServiceDoctorReport {
    let mut checks = vec![
        config_check,
        payload_doctor_check(paths),
        vcruntime_doctor_check(paths),
        ucrt_doctor_check(paths),
        service_health_doctor_check(paths),
        dbgeng_doctor_check(tools),
        ttd_doctor_check(tools),
        ida_doctor_check(runtime),
    ];
    let warnings = checks
        .iter()
        .filter(|check| check.status == DoctorCheckStatus::Warning)
        .map(|check| check.summary.clone())
        .collect::<Vec<_>>();
    let status = if warnings.is_empty() {
        DoctorCheckStatus::Ok
    } else {
        DoctorCheckStatus::Warning
    };
    WindowsServiceDoctorReport {
        status,
        install_root: paths.root_dir.clone(),
        checks: {
            checks.shrink_to_fit();
            checks
        },
        warnings,
        report_path,
        msi_summary_path,
    }
}

pub(super) fn payload_doctor_check(paths: &ServiceInstallPaths) -> DoctorCheck {
    let missing = WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES
        .iter()
        .map(|file_name| paths.bin_dir.join(file_name))
        .filter(|path| !path.is_file())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        doctor_check(
            "payload",
            "Release payload",
            DoctorCheckStatus::Ok,
            "required DbgAtlas payload files are present".to_string(),
            None,
            Some(paths.bin_dir.clone()),
        )
    } else {
        doctor_check(
            "payload",
            "Release payload",
            DoctorCheckStatus::Warning,
            format!("required payload files are missing: {}", missing.join(", ")),
            None,
            Some(paths.bin_dir.clone()),
        )
    }
}

pub(super) fn vcruntime_doctor_check(paths: &ServiceInstallPaths) -> DoctorCheck {
    match find_runtime_file("VCRUNTIME140.dll", paths) {
        Some(path) => doctor_check(
            "vcruntime",
            "Microsoft Visual C++ runtime",
            DoctorCheckStatus::Ok,
            "VCRUNTIME140.dll was found".to_string(),
            None,
            Some(path),
        ),
        None => doctor_check(
            "vcruntime",
            "Microsoft Visual C++ runtime",
            DoctorCheckStatus::Warning,
            "VCRUNTIME140.dll was not found; install the Microsoft Visual C++ Redistributable x64 if DbgAtlas fails to start".to_string(),
            None,
            None,
        ),
    }
}

pub(super) fn ucrt_doctor_check(paths: &ServiceInstallPaths) -> DoctorCheck {
    match find_runtime_file("ucrtbase.dll", paths) {
        Some(path) => doctor_check(
            "ucrt",
            "Universal C Runtime",
            DoctorCheckStatus::Ok,
            "ucrtbase.dll was found".to_string(),
            None,
            Some(path),
        ),
        None => doctor_check(
            "ucrt",
            "Universal C Runtime",
            DoctorCheckStatus::Warning,
            "ucrtbase.dll was not found; install current Windows updates or the Universal C Runtime if DbgAtlas fails to start".to_string(),
            None,
            None,
        ),
    }
}

pub(super) fn service_health_doctor_check(paths: &ServiceInstallPaths) -> DoctorCheck {
    let config = match installed_client_config_from_paths(paths) {
        Ok(Some(config)) => config,
        Ok(None) => {
            return doctor_check(
                "service_health",
                "Installed service health",
                DoctorCheckStatus::Warning,
                "installed service client config was not available; service.health was not checked"
                    .to_string(),
                None,
                Some(paths.root_dir.clone()),
            );
        }
        Err(error) => {
            return doctor_check(
                "service_health",
                "Installed service health",
                DoctorCheckStatus::Warning,
                format!("installed service client config could not be loaded: {error}"),
                None,
                Some(paths.root_dir.clone()),
            );
        }
    };

    let started = Instant::now();
    let timeout = Duration::from_secs(5);
    let mut last_error = None;
    while started.elapsed() <= timeout {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!("doctor-health")),
            method: "service.health".to_string(),
            params: Some(json!({})),
        };
        match invoke_http_json_rpc(config.bind, &config.bearer_token, &request) {
            Ok(response) if response.error.is_none() => {
                return doctor_check(
                    "service_health",
                    "Installed service health",
                    DoctorCheckStatus::Ok,
                    "service.health responded successfully".to_string(),
                    Some(format!("http://{}/rpc", config.bind)),
                    Some(paths.installed_exe.clone()),
                );
            }
            Ok(response) => {
                last_error = response.error.map(|error| error.message);
            }
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    doctor_check(
        "service_health",
        "Installed service health",
        DoctorCheckStatus::Warning,
        format!(
            "service.health did not respond successfully at http://{}/rpc within {} ms",
            config.bind,
            timeout.as_millis()
        ),
        last_error.map(|error| format!("last error: {error}")),
        Some(paths.installed_exe.clone()),
    )
}

pub(super) fn dbgeng_doctor_check(tools: &dbgatlas_runtime::ResolvedToolPaths) -> DoctorCheck {
    if tools.dbgeng_candidates.is_empty() {
        return doctor_check(
            "dbgeng",
            "WinDbg DbgEng runtime",
            DoctorCheckStatus::Warning,
            "dbgeng.dll was not detected; debug sessions may fail until WinDbg or Windows Debugging Tools are installed".to_string(),
            None,
            None,
        );
    }
    let detail = tools
        .dbgeng_candidates
        .iter()
        .map(|location| format!("{:?}: {}", location.source, location.dir.display()))
        .collect::<Vec<_>>()
        .join("; ");
    doctor_check(
        "dbgeng",
        "WinDbg DbgEng runtime",
        DoctorCheckStatus::Ok,
        "dbgeng.dll candidate was detected".to_string(),
        Some(detail),
        tools.dbgeng.as_ref().map(|location| location.dir.clone()),
    )
}

pub(super) fn ttd_doctor_check(tools: &dbgatlas_runtime::ResolvedToolPaths) -> DoctorCheck {
    match &tools.ttd {
        Some(location) => doctor_check(
            "ttd",
            "Time Travel Debugging",
            DoctorCheckStatus::Ok,
            "TTD.exe was found".to_string(),
            Some(format!("{:?}: {}", location.source, location.dir.display())),
            Some(location.dir.join("TTD.exe")),
        ),
        None => doctor_check(
            "ttd",
            "Time Travel Debugging",
            DoctorCheckStatus::Warning,
            "TTD.exe was not detected; TTD recording is unavailable until WinDbg/TTD is installed"
                .to_string(),
            None,
            None,
        ),
    }
}

pub(super) fn ida_doctor_check(runtime: &RuntimeConfig) -> DoctorCheck {
    let install_dir = runtime
        .tools
        .ida
        .install_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_IDA_INSTALL_DIR));
    let ida_dll = install_dir.join("ida.dll");
    let idalib_dll = install_dir.join("idalib.dll");
    if install_dir.is_dir() && ida_dll.is_file() && idalib_dll.is_file() {
        return doctor_check(
            "ida",
            "IDA Pro",
            DoctorCheckStatus::Ok,
            "IDA runtime files were found".to_string(),
            Some("ida.dll and idalib.dll are present".to_string()),
            Some(install_dir),
        );
    }
    let mut missing = Vec::new();
    if !install_dir.is_dir() {
        missing.push("install directory");
    }
    if !ida_dll.is_file() {
        missing.push("ida.dll");
    }
    if !idalib_dll.is_file() {
        missing.push("idalib.dll");
    }
    doctor_check(
        "ida",
        "IDA Pro",
        DoctorCheckStatus::Warning,
        format!(
            "IDA Pro 9.3 runtime was not detected at {}; reverse features are unavailable unless callers pass a valid ida_install_dir",
            install_dir.display()
        ),
        Some(format!("missing {}", missing.join(", "))),
        Some(install_dir),
    )
}

pub(super) fn doctor_check(
    id: &str,
    label: &str,
    status: DoctorCheckStatus,
    summary: String,
    detail: Option<String>,
    path: Option<PathBuf>,
) -> DoctorCheck {
    DoctorCheck {
        id: id.to_string(),
        label: label.to_string(),
        status,
        summary,
        detail,
        path,
    }
}

pub(super) fn find_runtime_file(file_name: &str, paths: &ServiceInstallPaths) -> Option<PathBuf> {
    runtime_search_dirs(paths)
        .into_iter()
        .map(|dir| dir.join(file_name))
        .find(|path| path.is_file())
}

pub(super) fn runtime_search_dirs(paths: &ServiceInstallPaths) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    push_unique_path(&mut dirs, paths.bin_dir.clone());
    if let Some(system_root) = std::env::var_os("SystemRoot").map(PathBuf::from) {
        push_unique_path(&mut dirs, system_root.join("System32"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            push_unique_path(&mut dirs, dir);
        }
    }
    dirs
}

pub fn format_doctor_report(report: &WindowsServiceDoctorReport) -> String {
    let mut text = String::new();
    text.push_str("DbgAtlas post-install diagnostic report\n");
    text.push_str(&format!("status: {}\n", report.status.label()));
    text.push_str(&format!(
        "install_root: {}\n",
        report.install_root.display()
    ));
    text.push_str("\nchecks:\n");
    for check in &report.checks {
        text.push_str(&format!(
            "- [{}] {}: {}\n",
            check.status.label(),
            check.label,
            check.summary
        ));
        if let Some(path) = &check.path {
            text.push_str(&format!("  path: {}\n", path.display()));
        }
        if let Some(detail) = &check.detail {
            text.push_str(&format!("  detail: {detail}\n"));
        }
    }
    if report.warnings.is_empty() {
        text.push_str("\nwarnings: none\n");
    } else {
        text.push_str("\nwarnings:\n");
        for warning in &report.warnings {
            text.push_str(&format!("- {warning}\n"));
        }
    }
    text
}

pub fn format_doctor_msi_summary(report: &WindowsServiceDoctorReport) -> String {
    let mut lines = Vec::new();
    if report.warnings.is_empty() {
        lines.push(
            "DbgAtlas installed successfully. No post-install diagnostic warnings were detected."
                .to_string(),
        );
    } else {
        lines.push("DbgAtlas installed successfully, with diagnostic warnings:".to_string());
        let warning_checks = report
            .checks
            .iter()
            .filter(|check| check.status == DoctorCheckStatus::Warning)
            .collect::<Vec<_>>();
        for check in warning_checks.iter().take(4) {
            lines.push(format!("- {}", doctor_msi_warning_line(check)));
        }
        if warning_checks.len() > 4 {
            lines.push(format!(
                "- {} additional warning(s); see the post-install report.",
                warning_checks.len() - 4
            ));
        }
    }
    if let Some(path) = &report.report_path {
        lines.push(format!("Details: {}", path.display()));
    }
    lines.join("\r\n")
}

pub(super) fn doctor_msi_warning_line(check: &DoctorCheck) -> String {
    match check.id.as_str() {
        "runtime_config" => {
            "Runtime config was not loaded; defaults were used for diagnostics.".to_string()
        }
        "payload" => "Required DbgAtlas payload files are missing.".to_string(),
        "vcruntime" => "Microsoft Visual C++ runtime was not detected.".to_string(),
        "ucrt" => "Universal C Runtime was not detected.".to_string(),
        "service_health" => "Installed service health check did not pass.".to_string(),
        "dbgeng" => "WinDbg DbgEng runtime was not detected; debug sessions may fail.".to_string(),
        "ttd" => "TTD.exe was not detected; TTD recording is unavailable.".to_string(),
        "ida" => {
            "IDA Pro 9.3 runtime was not detected; reverse features are unavailable.".to_string()
        }
        _ => format!("{}: {}", check.label, check.summary),
    }
}

pub(super) fn write_text_file(path: &Path, text: &str) -> Result<(), ServiceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}

pub fn install_windows_service(
    options: WindowsServiceInstallOptions,
) -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::install(options)
}

pub fn start_windows_service(
    options: WindowsServiceControlOptions,
) -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::start(options)
}

pub fn stop_windows_service(
    options: WindowsServiceControlOptions,
) -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::stop(options)
}

pub fn status_windows_service(
    options: WindowsServiceControlOptions,
) -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::status(options)
}

pub fn uninstall_windows_service(
    options: WindowsServiceUninstallOptions,
) -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::uninstall(options)
}

pub fn request_windows_service_update(
    options: WindowsServiceUpdateOptions,
) -> Result<WindowsServiceUpdateAccepted, ServiceError> {
    windows_service_control::request_update(options)
}

pub fn apply_windows_service_update(
    options: WindowsServiceApplyUpdateOptions,
) -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::apply_update(options)
}

pub fn run_windows_service_dispatcher(
    options: WindowsServiceRunOptions,
) -> Result<(), ServiceError> {
    windows_service_control::run_dispatcher(options)
}

pub(super) fn create_runtime_config_if_missing(
    paths: &ServiceInstallPaths,
    bind: SocketAddr,
) -> Result<RuntimeConfig, ServiceError> {
    fs::create_dir_all(&paths.etc_dir)?;
    if paths.config_path.exists() {
        return Ok(RuntimeConfig::load(&paths.config_path)?);
    }
    let mut runtime = RuntimeConfig::default();
    runtime.server.bind = bind;
    let config = runtime_config_toml(&runtime);
    let runtime = RuntimeConfig::from_toml_str(&config)?;
    fs::write(&paths.config_path, config)?;
    Ok(runtime)
}

pub(super) fn runtime_config_toml(runtime: &RuntimeConfig) -> String {
    let mut config = format!(
        "version = 1\n\n[server]\nbind = \"{}\"\n",
        runtime.server.bind
    );
    if runtime.tools.symbol_path.is_some() {
        config.push_str("\n[tools]\n");
        if let Some(symbol_path) = &runtime.tools.symbol_path {
            config.push_str(&format!("symbol_path = \"{}\"\n", toml_escape(symbol_path)));
        }
    }
    config
}

pub(super) fn toml_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

pub(super) fn prepare_install_layout(paths: &ServiceInstallPaths) -> Result<(), ServiceError> {
    fs::create_dir_all(&paths.root_dir)?;
    fs::create_dir_all(&paths.etc_dir)?;
    fs::create_dir_all(&paths.log_dir)?;
    fs::write(paths.install_marker_path(), "DbgAtlas install root\n")?;
    migrate_legacy_install_file(&paths.legacy_config_path(), &paths.config_path)?;
    migrate_legacy_install_file(&paths.legacy_token_file(), &paths.token_file)?;
    Ok(())
}

pub(super) fn migrate_legacy_install_file(
    legacy_path: &Path,
    target_path: &Path,
) -> Result<(), ServiceError> {
    if target_path.exists() || !legacy_path.exists() {
        return Ok(());
    }
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(legacy_path, target_path)?;
    Ok(())
}

pub(super) fn install_payload(
    payload: &[ServicePayloadFile],
    paths: &ServiceInstallPaths,
) -> Result<(), ServiceError> {
    if paths.staging_bin_dir.exists() {
        fs::remove_dir_all(&paths.staging_bin_dir)?;
    }
    fs::create_dir_all(&paths.staging_bin_dir)?;
    for file in payload {
        fs::copy(&file.source, paths.staging_bin_dir.join(&file.file_name))?;
    }
    if paths.bin_dir.exists() {
        fs::remove_dir_all(&paths.bin_dir)?;
    }
    fs::rename(&paths.staging_bin_dir, &paths.bin_dir)?;
    Ok(())
}

pub(super) fn validate_existing_payload(
    paths: &ServiceInstallPaths,
) -> Result<Vec<ServicePayloadFile>, ServiceError> {
    discover_service_payload(&paths.bin_dir, &paths.bin_dir)
}

pub(super) fn payload_dir_from_current_exe() -> Result<PathBuf, ServiceError> {
    let current_exe = std::env::current_exe()?;
    current_exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ServiceError::ServiceControl("current executable has no parent".into()))
}

pub(super) fn copy_missing_install_state_from_previous_root(
    paths: &ServiceInstallPaths,
) -> Result<(), ServiceError> {
    let previous = legacy_program_data_service_paths();
    copy_missing_install_state_from_paths(paths, &previous)
}

pub(super) fn copy_missing_install_state_from_paths(
    paths: &ServiceInstallPaths,
    previous: &ServiceInstallPaths,
) -> Result<(), ServiceError> {
    if previous.root_dir == paths.root_dir {
        return Ok(());
    }
    copy_missing_install_file(
        &[previous.config_path.clone(), previous.legacy_config_path()],
        &paths.config_path,
    )?;
    copy_missing_install_file(
        &[previous.token_file.clone(), previous.legacy_token_file()],
        &paths.token_file,
    )?;
    Ok(())
}

pub(super) fn copy_missing_install_file(
    candidates: &[PathBuf],
    target: &Path,
) -> Result<(), ServiceError> {
    if target.exists() {
        return Ok(());
    }
    let Some(source) = candidates.iter().find(|path| path.is_file()) else {
        return Ok(());
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, target)?;
    Ok(())
}

pub(super) fn cleanup_install_dirs(
    paths: &ServiceInstallPaths,
    purge: bool,
) -> Result<(), ServiceError> {
    if !purge {
        return Ok(());
    }
    validate_purge_install_root(paths)?;
    if paths.root_dir.exists() {
        fs::remove_dir_all(&paths.root_dir)?;
    }
    Ok(())
}

pub(super) fn validate_purge_install_root(paths: &ServiceInstallPaths) -> Result<(), ServiceError> {
    let root = normalize_install_root(&paths.root_dir)?;
    let has_marker = paths.install_marker_path().is_file();
    let known_unmarked_root = install_roots_match(&root, &default_windows_service_root())
        || install_roots_match(&root, &legacy_program_data_service_paths().root_dir);
    if !has_marker && !known_unmarked_root {
        return Err(ServiceError::ServiceControl(format!(
            "refusing to purge install root {}; expected a DbgAtlas install marker or a known default install root",
            root.display()
        )));
    }
    let canonical = fs::canonicalize(&root).unwrap_or(root.clone());
    if canonical.parent().is_none()
        || canonical
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .is_empty()
    {
        return Err(ServiceError::ServiceControl(format!(
            "refusing to purge unsafe install root {}",
            root.display()
        )));
    }
    Ok(())
}

pub(super) fn install_roots_match(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

pub(super) fn validate_update_timeout(timeout_ms: u64) -> Result<Duration, ServiceError> {
    if timeout_ms == 0 {
        return Err(ServiceError::Rpc(
            "timeout_ms must be greater than 0".to_string(),
        ));
    }
    Ok(Duration::from_millis(timeout_ms))
}

pub(super) fn prepare_service_update(
    source_dir: &Path,
    paths: &ServiceInstallPaths,
    restart: bool,
) -> Result<PreparedServiceUpdate, ServiceError> {
    let source_dir = fs::canonicalize(source_dir)?;
    if !source_dir.is_dir() {
        return Err(ServiceError::Rpc(format!(
            "source_dir is not a directory: {}",
            source_dir.display()
        )));
    }
    if source_is_installed_bin(&source_dir, paths) {
        return Err(ServiceError::ServiceControl(
            "cannot update service from the installed bin directory; pass a development or release payload directory".to_string(),
        ));
    }
    let bin_payload = discover_service_payload(&source_dir, &paths.bin_dir)?;
    let config_payload = discover_service_config_payload(&source_dir, paths)?;
    let mut response_payload = bin_payload.clone();
    if let Some(config_payload) = &config_payload {
        response_payload.push(config_payload.clone());
    }
    let response = WindowsServiceUpdateAccepted {
        status: "accepted".to_string(),
        source_dir: source_dir.clone(),
        service_name: WINDOWS_SERVICE_NAME.to_string(),
        installed_binary: paths.installed_exe.clone(),
        log_dir: paths.log_dir.clone(),
        payload: response_payload,
        restart,
    };
    Ok(PreparedServiceUpdate {
        source_dir,
        bin_payload,
        config_payload,
        response,
    })
}

pub(super) fn discover_service_config_payload(
    source_dir: &Path,
    paths: &ServiceInstallPaths,
) -> Result<Option<ServicePayloadFile>, ServiceError> {
    // release payload 可以携带新的 runtime.toml 来调整 bind/proxy/工具策略；
    // token 是机器本地密钥，只能留在安装态 etc\token，不能由 payload 覆盖。
    let candidates = [
        source_dir.join(WINDOWS_SERVICE_CONFIG_FILE),
        source_dir
            .join(WINDOWS_SERVICE_ETC_DIR)
            .join(WINDOWS_SERVICE_CONFIG_FILE),
    ];
    let found = candidates
        .iter()
        .filter(|path| path.is_file())
        .cloned()
        .collect::<Vec<_>>();
    match found.as_slice() {
        [] => Ok(None),
        [source] => {
            validate_update_runtime_config(source)?;
            Ok(Some(ServicePayloadFile {
                file_name: WINDOWS_SERVICE_CONFIG_FILE.to_string(),
                source: source.clone(),
                destination: paths.config_path.clone(),
            }))
        }
        _ => Err(ServiceError::Rpc(format!(
            "provide {WINDOWS_SERVICE_CONFIG_FILE} in either the payload root or {WINDOWS_SERVICE_ETC_DIR}\\{WINDOWS_SERVICE_CONFIG_FILE}, not both"
        ))),
    }
}

pub(super) fn validate_update_runtime_config(path: &Path) -> Result<(), ServiceError> {
    // 先按 TOML 结构拒绝任何 token 形态，再交给 dbgatlas-runtime 做版本和字段校验。
    // 这样 updater 即使被传入错误 payload，也不会把 bearer token 写进 runtime.toml。
    let input = fs::read_to_string(path)?;
    reject_runtime_config_token_keys(&input)?;
    reject_runtime_config_local_debug_tool_keys(&input)?;
    let _runtime = RuntimeConfig::from_toml_str(&input)?;
    Ok(())
}

pub(super) fn reject_runtime_config_token_keys(input: &str) -> Result<(), ServiceError> {
    let value = toml::from_str::<toml::Value>(input)
        .map_err(dbgatlas_runtime::RuntimeConfigError::ParseToml)?;
    let mut path = Vec::new();
    reject_runtime_config_token_keys_in_value(&value, &mut path)
}

pub(super) fn reject_runtime_config_token_keys_in_value(
    value: &toml::Value,
    path: &mut Vec<String>,
) -> Result<(), ServiceError> {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                path.push(key.clone());
                if is_runtime_config_token_key(key) {
                    return Err(ServiceError::Rpc(format!(
                        "runtime config payload must not contain token fields: {}",
                        path.join(".")
                    )));
                }
                reject_runtime_config_token_keys_in_value(value, path)?;
                path.pop();
            }
            Ok(())
        }
        toml::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                path.push(index.to_string());
                reject_runtime_config_token_keys_in_value(value, path)?;
                path.pop();
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn is_runtime_config_token_key(key: &str) -> bool {
    key.chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .collect::<String>()
        .to_ascii_lowercase()
        .contains("token")
}

pub(super) fn reject_runtime_config_local_debug_tool_keys(input: &str) -> Result<(), ServiceError> {
    let value = toml::from_str::<toml::Value>(input)
        .map_err(dbgatlas_runtime::RuntimeConfigError::ParseToml)?;
    let tools = value
        .as_table()
        .and_then(|table| table.get("tools"))
        .and_then(toml::Value::as_table);
    if let Some(tools) = tools {
        for key in ["dbgeng_dir", "ttd_dir"] {
            if tools.contains_key(key) {
                return Err(ServiceError::Rpc(format!(
                    "runtime config payload must not contain removed local debug tool field: tools.{key}"
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn copy_update_payload_to_staging(
    payload: &[ServicePayloadFile],
    staging_dir: &Path,
) -> Result<(), ServiceError> {
    if staging_dir.exists() {
        fs::remove_dir_all(staging_dir)?;
    }
    fs::create_dir_all(staging_dir)?;
    for file in payload {
        fs::copy(&file.source, staging_dir.join(&file.file_name))?;
    }
    Ok(())
}

pub(super) fn copy_update_config_to_staging(
    config_payload: &ServicePayloadFile,
    staging_dir: &Path,
    paths: &ServiceInstallPaths,
) -> Result<ServicePayloadFile, ServiceError> {
    if config_payload.destination != paths.config_path {
        return Err(ServiceError::ServiceControl(format!(
            "refusing to stage service config for unexpected path: {}",
            config_payload.destination.display()
        )));
    }
    fs::create_dir_all(staging_dir)?;
    let staged = staging_dir.join(WINDOWS_SERVICE_CONFIG_FILE);
    fs::copy(&config_payload.source, &staged)?;
    validate_update_runtime_config(&staged)?;
    Ok(ServicePayloadFile {
        file_name: WINDOWS_SERVICE_CONFIG_FILE.to_string(),
        source: staged,
        destination: paths.config_path.clone(),
    })
}

pub(super) fn apply_update_config_payload(
    config_payload: &ServicePayloadFile,
    paths: &ServiceInstallPaths,
) -> Result<(), ServiceError> {
    if config_payload.destination != paths.config_path {
        return Err(ServiceError::ServiceControl(format!(
            "refusing to copy service config to unexpected path: {}",
            config_payload.destination.display()
        )));
    }
    validate_update_runtime_config(&config_payload.source)?;
    fs::create_dir_all(&paths.etc_dir)?;
    fs::copy(&config_payload.source, &paths.config_path)?;
    Ok(())
}

pub(super) fn replace_installed_bin_with_staging(
    paths: &ServiceInstallPaths,
    staging_dir: &Path,
    suffix: &str,
    timeout: Duration,
) -> Result<PathBuf, ServiceError> {
    let old_dir = paths.root_dir.join(format!("bin.old-{suffix}"));
    if old_dir.exists() {
        fs::remove_dir_all(&old_dir)?;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let result = replace_installed_bin_once(paths, staging_dir, &old_dir);
        match result {
            Ok(()) => return Ok(old_dir),
            Err(_error) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(250));
                if !paths.bin_dir.exists() && old_dir.exists() {
                    let _ = fs::rename(&old_dir, &paths.bin_dir);
                }
                continue;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn replace_installed_bin_once(
    paths: &ServiceInstallPaths,
    staging_dir: &Path,
    old_dir: &Path,
) -> Result<(), ServiceError> {
    if !old_dir.exists() && paths.bin_dir.exists() {
        fs::rename(&paths.bin_dir, old_dir)?;
    }
    match fs::rename(staging_dir, &paths.bin_dir) {
        Ok(()) => Ok(()),
        Err(error) => {
            if old_dir.exists() && !paths.bin_dir.exists() {
                let _ = fs::rename(old_dir, &paths.bin_dir);
            }
            Err(error.into())
        }
    }
}

pub(super) fn cleanup_update_dirs(paths: &ServiceInstallPaths) -> Result<(), ServiceError> {
    if !paths.root_dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&paths.root_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("bin.old-") || name.starts_with("bin.next-") || name == "bin.staging" {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

pub(super) fn source_is_installed_bin(source_dir: &Path, paths: &ServiceInstallPaths) -> bool {
    let Ok(source) = fs::canonicalize(source_dir) else {
        return false;
    };
    let Ok(destination) = fs::canonicalize(&paths.bin_dir) else {
        return false;
    };
    source == destination
}

#[cfg(not(windows))]
pub(super) mod windows_service_control {
    use super::*;

    pub fn install(
        _options: WindowsServiceInstallOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn start(
        _options: WindowsServiceControlOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn stop(
        _options: WindowsServiceControlOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn status(
        _options: WindowsServiceControlOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn uninstall(
        _options: WindowsServiceUninstallOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn request_update(
        _options: WindowsServiceUpdateOptions,
    ) -> Result<WindowsServiceUpdateAccepted, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn apply_update(
        _options: WindowsServiceApplyUpdateOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn run_dispatcher(_options: WindowsServiceRunOptions) -> Result<(), ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }
}

#[cfg(windows)]
pub(super) mod windows_service_control {
    use super::*;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::OnceLock;
    use std::time::Instant;
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ,
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    windows_service::define_windows_service!(ffi_service_main, service_main);

    static RUN_OPTIONS: OnceLock<WindowsServiceRunOptions> = OnceLock::new();
    const MACHINE_ENVIRONMENT_REGISTRY_KEY: &str =
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

    pub fn install(
        options: WindowsServiceInstallOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        let paths = windows_service_paths_for_install(options.install_root.as_deref())?;
        set_process_service_install_root(paths.root_dir.clone())?;
        if options.payload_mode == WindowsServicePayloadMode::UseExisting
            && options.payload_dir.is_some()
        {
            return Err(ServiceError::ServiceControl(
                "--payload-dir is only valid with --payload-mode copy".to_string(),
            ));
        }
        let payload = match options.payload_mode {
            WindowsServicePayloadMode::Copy => {
                let source_dir = options
                    .payload_dir
                    .clone()
                    .unwrap_or(payload_dir_from_current_exe()?);
                if source_is_installed_bin(&source_dir, &paths) {
                    return Err(ServiceError::ServiceControl(
                        "cannot copy service payload from the installed bin directory; pass a separate --payload-dir or use --payload-mode use-existing".to_string(),
                    ));
                }
                discover_service_payload(&source_dir, &paths.bin_dir)?
            }
            WindowsServicePayloadMode::UseExisting => validate_existing_payload(&paths)?,
        };

        let manager =
            manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        let mut previous_paths = None;
        if let Some(service) = open_optional(
            &manager,
            ServiceAccess::QUERY_STATUS
                | ServiceAccess::QUERY_CONFIG
                | ServiceAccess::CHANGE_CONFIG
                | ServiceAccess::STOP,
        )? {
            previous_paths = previous_install_paths_from_service(&service).ok();
            let status = service.query_status().map_err(map_windows_service_error)?;
            if status.current_state != ServiceState::Stopped {
                if !options.force {
                    return Err(ServiceError::ServiceIsRunning);
                }
                stop_service(&service)?;
            }
            if !options.force {
                return Err(ServiceError::ServiceControl(
                    "service is already installed; use `dbgatlas service install --force` to update payload and service entry".to_string(),
                ));
            }
        }
        let legacy_program_data_paths = legacy_program_data_service_paths();
        let mut windbg_runtime_candidates = windbg_runtime_source_candidates(&paths);
        if let Some(previous_paths) = &previous_paths {
            append_windbg_runtime_source_candidates(&mut windbg_runtime_candidates, previous_paths);
        }
        append_windbg_runtime_source_candidates(
            &mut windbg_runtime_candidates,
            &legacy_program_data_paths,
        );

        prepare_install_layout(&paths)?;
        if let Some(previous_paths) = &previous_paths {
            copy_missing_install_state_from_paths(&paths, previous_paths)?;
        }
        copy_missing_install_state_from_previous_root(&paths)?;
        let runtime = create_runtime_config_if_missing(&paths, options.bind)?;
        ensure_install_token(&paths.token_file)?;
        if options.payload_mode == WindowsServicePayloadMode::Copy {
            install_payload(&payload, &paths)?;
        }
        let suffix = update_dir_suffix();
        let windbg_runtime = prepare_store_windbg_runtime_staging(&paths, &suffix)?;
        if let Some(windbg_runtime) = &windbg_runtime {
            activate_staged_windbg_runtime(windbg_runtime, &suffix)?;
        } else {
            let _ = copy_existing_windbg_runtime_to_destination(
                &windbg_runtime_candidates,
                &paths.windbg_runtime_dir,
            )?;
        }
        create_or_update_service(&manager, &paths)?;

        Ok(result(
            "installed",
            Some(runtime.server.bind),
            paths,
            payload,
        ))
    }

    pub fn start(
        options: WindowsServiceControlOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        let paths = windows_service_paths_for_root(options.install_root.as_deref())?;
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = open_required(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::QUERY_CONFIG,
        )?;
        let status = service.query_status().map_err(map_windows_service_error)?;
        if status.current_state != ServiceState::Running {
            service
                .start::<OsString>(&[])
                .map_err(map_windows_service_error)?;
            wait_for_state(&service, ServiceState::Running)?;
        }
        Ok(result(
            "running",
            installed_endpoint(&paths)?,
            paths,
            Vec::new(),
        ))
    }

    pub fn stop(
        options: WindowsServiceControlOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        let paths = windows_service_paths_for_root(options.install_root.as_deref())?;
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = open_required(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::QUERY_CONFIG,
        )?;
        stop_service(&service)?;
        Ok(result(
            "stopped",
            installed_endpoint(&paths)?,
            paths,
            Vec::new(),
        ))
    }

    pub fn status(
        options: WindowsServiceControlOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        let paths = windows_service_paths_for_root(options.install_root.as_deref())?;
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let Some(service) = open_optional(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG,
        )?
        else {
            return Ok(result(
                "not_installed",
                installed_endpoint(&paths).ok().flatten(),
                paths,
                Vec::new(),
            ));
        };
        let status = service.query_status().map_err(map_windows_service_error)?;
        Ok(result(
            state_name(status.current_state),
            installed_endpoint(&paths)?,
            paths,
            Vec::new(),
        ))
    }

    pub fn uninstall(
        options: WindowsServiceUninstallOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        let paths = windows_service_paths_for_root(options.install_root.as_deref())?;
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let Some(service) = open_optional(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )?
        else {
            if options.purge {
                cleanup_install_dirs(&paths, true)?;
            }
            return Ok(result("not_installed", None, paths, Vec::new()));
        };
        stop_service(&service)?;
        service.delete().map_err(map_windows_service_error)?;
        cleanup_install_dirs(&paths, options.purge)?;
        Ok(result("uninstalled", None, paths, Vec::new()))
    }

    pub fn request_update(
        options: WindowsServiceUpdateOptions,
    ) -> Result<WindowsServiceUpdateAccepted, ServiceError> {
        validate_update_timeout(options.timeout_ms)?;
        let paths = default_windows_service_paths();
        let prepared = prepare_service_update(&options.source_dir, &paths, options.restart)?;
        let updater_exe = prepared.source_dir.join("dbgatlas.exe");
        let mut command = Command::new(updater_exe);
        command
            .arg("service")
            .arg("apply-update")
            .arg("--install-root")
            .arg(&paths.root_dir)
            .arg("--source-dir")
            .arg(&prepared.source_dir)
            .arg("--timeout-ms")
            .arg(options.timeout_ms.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if !options.restart {
            command.arg("--no-restart");
        }
        let child = command.spawn()?;
        append_service_log(&format!(
            "accepted service.update from {}; updater pid={} restart={} timeout_ms={} payload_files={} config_payload={}",
            prepared.source_dir.display(),
            child.id(),
            options.restart,
            options.timeout_ms,
            prepared.bin_payload.len(),
            prepared.config_payload.is_some()
        ));
        Ok(prepared.response)
    }

    pub fn apply_update(
        options: WindowsServiceApplyUpdateOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        let timeout = validate_update_timeout(options.timeout_ms)?;
        let deadline = Instant::now() + timeout;
        let paths = windows_service_paths_for_root(options.install_root.as_deref())?;
        set_process_service_install_root(paths.root_dir.clone())?;
        prepare_install_layout(&paths)?;
        let prepared = prepare_service_update(&options.source_dir, &paths, options.restart)?;
        let suffix = update_dir_suffix();
        let staging_dir = paths.root_dir.join(format!("bin.next-{suffix}"));
        append_service_log(&format!(
            "starting service apply-update from {}; staging {}; restart={} timeout_ms={} payload_files={} config_payload={}",
            prepared.source_dir.display(),
            staging_dir.display(),
            options.restart,
            options.timeout_ms,
            prepared.bin_payload.len(),
            prepared.config_payload.is_some()
        ));
        copy_update_payload_to_staging(&prepared.bin_payload, &staging_dir)?;
        let staged_windbg_runtime_dir =
            prepare_update_windbg_runtime_in_staging(&paths, &staging_dir, &suffix)?;
        let staged_config_payload = prepared
            .config_payload
            .as_ref()
            .map(|config_payload| {
                copy_update_config_to_staging(config_payload, &staging_dir, &paths)
            })
            .transpose()?;

        if let Some(config_payload) = &staged_config_payload {
            apply_update_config_payload(config_payload, &paths)?;
            append_service_log(&format!(
                "service runtime config replaced from {}",
                config_payload.source.display()
            ));
        }

        std::thread::sleep(Duration::from_millis(SERVICE_UPDATE_DELAY_MS));
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let service = open_required(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::START,
        )?;
        stop_service_with_timeout(&service, remaining_update_timeout(deadline)?)?;
        append_service_log("service stopped for apply-update");

        let old_dir = replace_installed_bin_with_staging(
            &paths,
            &staging_dir,
            &suffix,
            remaining_update_timeout(deadline)?,
        )?;
        append_service_log(&format!(
            "service payload replaced; previous bin at {}",
            old_dir.display()
        ));
        if let Some(windbg_runtime_dir) = &staged_windbg_runtime_dir {
            append_service_log(&format!(
                "service WinDbg runtime staged at {}",
                windbg_runtime_dir.display()
            ));
        }

        if options.restart {
            start_service_with_timeout(&service, remaining_update_timeout(deadline)?)?;
            append_service_log("service restarted after apply-update");
        } else {
            append_service_log("service restart skipped after apply-update");
        }

        if let Err(error) = cleanup_update_dirs(&paths) {
            append_service_log(&format!("service update cleanup failed: {error}"));
        }
        if let Err(error) = cleanup_windbg_runtime_update_dirs(&paths) {
            append_service_log(&format!("WinDbg runtime update cleanup failed: {error}"));
        }

        Ok(result(
            if options.restart {
                "running"
            } else {
                "stopped"
            },
            installed_endpoint(&paths).ok().flatten(),
            paths,
            prepared.response.payload,
        ))
    }

    pub fn run_dispatcher(options: WindowsServiceRunOptions) -> Result<(), ServiceError> {
        set_process_service_install_root(options.install_root.clone())?;
        append_service_log("starting Windows service dispatcher");
        RUN_OPTIONS.set(options).map_err(|_| {
            ServiceError::ServiceControl("service run options were already set".to_string())
        })?;
        let result =
            windows_service::service_dispatcher::start(WINDOWS_SERVICE_NAME, ffi_service_main)
                .map_err(map_windows_service_error);
        if let Err(error) = &result {
            append_service_log(&format!("service dispatcher failed: {error}"));
        }
        result
    }

    pub(super) fn service_main(_arguments: Vec<OsString>) {
        append_service_log("entered service_main");
        if let Err(error) = run_service_main() {
            append_service_log(&format!("service_main failed: {error}"));
            eprintln!("DbgAtlas Windows service error: {error:#}");
        }
        append_service_log("leaving service_main");
    }

    pub(super) fn run_service_main() -> Result<(), ServiceError> {
        append_service_log("loading service run options");
        let options = RUN_OPTIONS.get().cloned().ok_or_else(|| {
            ServiceError::ServiceControl("missing service run options".to_string())
        })?;
        let paths = ServiceInstallPaths::for_root(options.install_root.clone());
        let shutdown = ServiceShutdown::new();
        let stop_signal = shutdown.clone();
        let event_handler = move |control_event| -> ServiceControlHandlerResult {
            match control_event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    stop_signal.request_stop();
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        append_service_log("registering service control handler");
        let status_handle = service_control_handler::register(WINDOWS_SERVICE_NAME, event_handler)
            .map_err(map_windows_service_error)?;
        append_service_log("registered service control handler");
        set_status(&status_handle, ServiceState::StartPending)?;
        append_service_log("reported start_pending");
        let runtime = RuntimeConfig::load(&options.config_path)?;
        let bearer_token = fs::read_to_string(&options.token_file)?.trim().to_string();
        let config = ServiceConfig {
            bind: runtime.server.bind,
            bearer_token,
        };
        validate_config(&config)?;
        set_status(&status_handle, ServiceState::Running)?;
        append_service_log(&format!("reported running on {}", config.bind));
        let result = run_http_service_until(
            config,
            ServiceHost::with_installed_process_workers()?.with_capabilities(
                ServiceCapabilities::from_runtime_config_with_install_paths(&runtime, &paths),
            ),
            shutdown,
        );
        set_status(&status_handle, ServiceState::Stopped)?;
        append_service_log("reported stopped");
        result
    }

    pub(super) fn create_or_update_service(
        manager: &ServiceManager,
        paths: &ServiceInstallPaths,
    ) -> Result<(), ServiceError> {
        let info = service_info(paths);
        if let Some(service) = open_optional(manager, ServiceAccess::CHANGE_CONFIG)? {
            service
                .change_config(&info)
                .map_err(map_windows_service_error)?;
            service
                .set_description(WINDOWS_SERVICE_DESCRIPTION)
                .map_err(map_windows_service_error)?;
            return Ok(());
        }
        let service = manager
            .create_service(
                &info,
                ServiceAccess::QUERY_STATUS | ServiceAccess::CHANGE_CONFIG,
            )
            .map_err(map_windows_service_error)?;
        service
            .set_description(WINDOWS_SERVICE_DESCRIPTION)
            .map_err(map_windows_service_error)?;
        Ok(())
    }

    pub(super) fn previous_install_paths_from_service(
        service: &windows_service::service::Service,
    ) -> Result<ServiceInstallPaths, ServiceError> {
        let config = service.query_config().map_err(map_windows_service_error)?;
        service_paths_from_scm_binary_path(&config.executable_path)
    }

    pub(super) fn service_info(paths: &ServiceInstallPaths) -> ServiceInfo {
        ServiceInfo {
            name: OsString::from(WINDOWS_SERVICE_NAME),
            display_name: OsString::from(WINDOWS_SERVICE_DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::OnDemand,
            error_control: ServiceErrorControl::Normal,
            executable_path: paths.installed_exe.clone(),
            launch_arguments: vec![
                OsString::from("service"),
                OsString::from("run"),
                OsString::from("--windows-service"),
                OsString::from("--install-root"),
                paths.root_dir.clone().into_os_string(),
                OsString::from("--config"),
                paths.config_path.clone().into_os_string(),
                OsString::from("--token-file"),
                paths.token_file.clone().into_os_string(),
            ],
            dependencies: Vec::new(),
            account_name: None,
            account_password: None,
        }
    }

    pub(super) fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, ServiceError> {
        ServiceManager::local_computer(None::<&str>, access).map_err(map_windows_service_error)
    }

    pub(super) fn open_required(
        manager: &ServiceManager,
        access: ServiceAccess,
    ) -> Result<windows_service::service::Service, ServiceError> {
        open_optional(manager, access)?.ok_or_else(|| {
            ServiceError::ServiceControl(format!(
                "Windows service `{WINDOWS_SERVICE_NAME}` is not installed"
            ))
        })
    }

    pub(super) fn open_optional(
        manager: &ServiceManager,
        access: ServiceAccess,
    ) -> Result<Option<windows_service::service::Service>, ServiceError> {
        match manager.open_service(WINDOWS_SERVICE_NAME, access) {
            Ok(service) => Ok(Some(service)),
            Err(error) if is_service_not_found(&error) => Ok(None),
            Err(error) => Err(map_windows_service_error(error)),
        }
    }

    pub(super) fn stop_service(
        service: &windows_service::service::Service,
    ) -> Result<(), ServiceError> {
        stop_service_with_timeout(service, Duration::from_secs(15))
    }

    pub(super) fn stop_service_with_timeout(
        service: &windows_service::service::Service,
        timeout: Duration,
    ) -> Result<(), ServiceError> {
        let status = service.query_status().map_err(map_windows_service_error)?;
        if status.current_state == ServiceState::Stopped {
            return Ok(());
        }
        match service.stop() {
            Ok(_) => wait_for_state_with_timeout(service, ServiceState::Stopped, timeout),
            Err(error) if is_service_not_active(&error) => Ok(()),
            Err(error) => Err(map_windows_service_error(error)),
        }
    }

    pub(super) fn start_service_with_timeout(
        service: &windows_service::service::Service,
        timeout: Duration,
    ) -> Result<(), ServiceError> {
        let status = service.query_status().map_err(map_windows_service_error)?;
        if status.current_state == ServiceState::Running {
            return Ok(());
        }
        service
            .start::<OsString>(&[])
            .map_err(map_windows_service_error)?;
        wait_for_state_with_timeout(service, ServiceState::Running, timeout)
    }

    pub(super) fn wait_for_state(
        service: &windows_service::service::Service,
        expected: ServiceState,
    ) -> Result<(), ServiceError> {
        wait_for_state_with_timeout(service, expected, Duration::from_secs(15))
    }

    pub(super) fn wait_for_state_with_timeout(
        service: &windows_service::service::Service,
        expected: ServiceState,
        timeout: Duration,
    ) -> Result<(), ServiceError> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = service.query_status().map_err(map_windows_service_error)?;
            if status.current_state == expected {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ServiceError::ServiceControl(format!(
                    "timed out waiting for service state `{}`; current state is `{}`",
                    state_name(expected),
                    state_name(status.current_state)
                )));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    pub(super) fn remaining_update_timeout(deadline: Instant) -> Result<Duration, ServiceError> {
        deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                ServiceError::ServiceControl("timed out applying service update".to_string())
            })
    }

    pub(super) fn update_dir_suffix() -> String {
        format!("{}-{}", Timestamp::now().unix_millis, std::process::id())
    }

    pub(super) fn set_status(
        status_handle: &ServiceStatusHandle,
        current_state: ServiceState,
    ) -> Result<(), ServiceError> {
        let controls_accepted = if current_state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        };
        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state,
                controls_accepted,
                exit_code: ServiceExitCode::NO_ERROR,
                checkpoint: 0,
                wait_hint: Duration::from_secs(10),
                process_id: None,
            })
            .map_err(map_windows_service_error)
    }

    pub(super) fn ensure_install_token(path: &Path) -> Result<(), ServiceError> {
        let system_env_token = read_machine_environment_variable(DBGATLAS_TOKEN_ENV_VAR)?;
        let file_token = read_install_token_file(path)?;
        let selection = resolve_install_token(
            system_env_token.as_deref(),
            file_token.as_deref(),
            generate_token,
        )?;
        if selection.write_system_env {
            write_machine_environment_variable(DBGATLAS_TOKEN_ENV_VAR, &selection.token)?;
            broadcast_environment_change();
        }
        write_install_token_file(path, &selection.token)
    }

    pub(super) fn read_install_token_file(path: &Path) -> Result<Option<String>, ServiceError> {
        if path.exists() {
            let token = fs::read_to_string(path)?;
            return Ok(trimmed_non_empty_token(Some(&token)));
        }
        Ok(None)
    }

    pub(super) fn write_install_token_file(path: &Path, token: &str) -> Result<(), ServiceError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, format!("{token}\n"))?;
        Ok(())
    }

    pub(super) fn read_machine_environment_variable(
        name: &str,
    ) -> Result<Option<String>, ServiceError> {
        let key = RegistryKey::open_machine_environment(KEY_QUERY_VALUE)?;
        let name = wide_null(name);
        let mut value_type = 0u32;
        let mut byte_len = 0u32;
        let status = unsafe {
            RegQueryValueExW(
                key.raw(),
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut value_type,
                std::ptr::null_mut(),
                &mut byte_len,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
            return Err(registry_error(
                "query machine environment variable size",
                status,
            ));
        }
        if value_type != REG_SZ && value_type != REG_EXPAND_SZ {
            return Err(ServiceError::ServiceControl(format!(
                "machine environment variable {DBGATLAS_TOKEN_ENV_VAR} has unsupported registry type {value_type}"
            )));
        }

        let mut bytes = vec![0u8; byte_len as usize];
        let status = unsafe {
            RegQueryValueExW(
                key.raw(),
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut value_type,
                bytes.as_mut_ptr(),
                &mut byte_len,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(registry_error("query machine environment variable", status));
        }
        bytes.truncate(byte_len as usize);
        utf16_registry_string(&bytes).map(Some)
    }

    pub(super) fn write_machine_environment_variable(
        name: &str,
        value: &str,
    ) -> Result<(), ServiceError> {
        let key = RegistryKey::open_machine_environment(KEY_SET_VALUE)?;
        let name = wide_null(name);
        let value = wide_null(value);
        let byte_len = value
            .len()
            .checked_mul(std::mem::size_of::<u16>())
            .and_then(|len| u32::try_from(len).ok())
            .ok_or_else(|| {
                ServiceError::ServiceControl(
                    "machine environment variable value is too large".to_string(),
                )
            })?;
        let status = unsafe {
            RegSetValueExW(
                key.raw(),
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast::<u8>(),
                byte_len,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(registry_error("write machine environment variable", status));
        }
        Ok(())
    }

    struct RegistryKey(HKEY);

    impl RegistryKey {
        fn open_machine_environment(access: u32) -> Result<Self, ServiceError> {
            let subkey = wide_null(MACHINE_ENVIRONMENT_REGISTRY_KEY);
            let mut key = std::ptr::null_mut();
            let status =
                unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, access, &mut key) };
            if status != ERROR_SUCCESS {
                return Err(registry_error(
                    "open machine environment registry key",
                    status,
                ));
            }
            Ok(Self(key))
        }

        fn raw(&self) -> HKEY {
            self.0
        }
    }

    impl Drop for RegistryKey {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    RegCloseKey(self.0);
                }
            }
        }
    }

    pub(super) fn utf16_registry_string(bytes: &[u8]) -> Result<String, ServiceError> {
        let mut chunks = bytes.chunks_exact(2);
        let mut units = chunks
            .by_ref()
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        if !chunks.remainder().is_empty() {
            return Err(ServiceError::ServiceControl(
                "machine environment variable contains invalid UTF-16 bytes".to_string(),
            ));
        }
        while units.last() == Some(&0) {
            units.pop();
        }
        String::from_utf16(&units).map_err(|error| {
            ServiceError::ServiceControl(format!(
                "machine environment variable contains invalid UTF-16: {error}"
            ))
        })
    }

    pub(super) fn wide_null(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn broadcast_environment_change() {
        let environment = wide_null("Environment");
        let mut result = 0usize;
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                environment.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
        }
    }

    pub(super) fn registry_error(operation: &str, status: u32) -> ServiceError {
        ServiceError::ServiceControl(format!("{operation} failed with Win32 status {status}"))
    }

    pub(super) fn generate_token() -> Result<String, ServiceError> {
        use windows_sys::Win32::Security::Cryptography::{
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
        };

        let mut bytes = [0u8; 32];
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(ServiceError::ServiceControl(format!(
                "BCryptGenRandom failed with status {status}"
            )));
        }
        Ok(hex_encode(&bytes))
    }

    pub(super) fn installed_endpoint(
        paths: &ServiceInstallPaths,
    ) -> Result<Option<SocketAddr>, ServiceError> {
        Ok(installed_client_config_from_paths(paths)?.map(|config| config.bind))
    }

    pub(super) fn result(
        status: &str,
        endpoint: Option<SocketAddr>,
        paths: ServiceInstallPaths,
        payload: Vec<ServicePayloadFile>,
    ) -> WindowsServiceCommandResult {
        WindowsServiceCommandResult {
            service_name: WINDOWS_SERVICE_NAME.to_string(),
            display_name: WINDOWS_SERVICE_DISPLAY_NAME.to_string(),
            status: status.to_string(),
            endpoint,
            installed_binary: paths.installed_exe,
            config_path: paths.config_path,
            token_file: paths.token_file,
            log_dir: paths.log_dir,
            payload,
        }
    }

    pub(super) fn state_name(state: ServiceState) -> &'static str {
        match state {
            ServiceState::Stopped => "stopped",
            ServiceState::StartPending => "start_pending",
            ServiceState::StopPending => "stop_pending",
            ServiceState::Running => "running",
            ServiceState::ContinuePending => "continue_pending",
            ServiceState::PausePending => "pause_pending",
            ServiceState::Paused => "paused",
        }
    }

    pub(super) fn hex_encode(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
        output
    }

    pub(super) fn is_service_not_found(error: &windows_service::Error) -> bool {
        matches!(
            error,
            windows_service::Error::Winapi(io) if io.raw_os_error() == Some(1060)
        )
    }

    pub(super) fn is_service_not_active(error: &windows_service::Error) -> bool {
        matches!(
            error,
            windows_service::Error::Winapi(io) if io.raw_os_error() == Some(1062)
        )
    }

    pub(super) fn map_windows_service_error(error: windows_service::Error) -> ServiceError {
        if let windows_service::Error::Winapi(io) = &error {
            if io.raw_os_error() == Some(5) {
                return ServiceError::ServiceControl(
                    "access denied; run this command from an elevated Administrator shell"
                        .to_string(),
                );
            }
        }
        ServiceError::ServiceControl(format!("{error:?}"))
    }

    pub(super) fn append_service_log(message: &str) {
        let paths = default_windows_service_paths();
        let timestamp = Timestamp::now().unix_millis;
        let day = (timestamp / 86_400_000) as i64;
        let line = format!("{timestamp} {message}\n");
        let log_path = paths
            .log_dir
            .join(format!("service-{}.log", utc_date_from_unix_day(day)));
        let _ = fs::create_dir_all(&paths.log_dir);
        let _ = prune_service_logs(&paths.log_dir, day);
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .and_then(|mut file| file.write_all(line.as_bytes()));
    }

    pub(super) fn prune_service_logs(log_dir: &Path, current_day: i64) -> Result<(), ServiceError> {
        let cutoff_day = current_day - WINDOWS_SERVICE_LOG_RETENTION_DAYS + 1;
        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(day) = service_log_file_day(file_name) else {
                continue;
            };
            if day < cutoff_day {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub(super) fn service_log_file_day(file_name: &str) -> Option<i64> {
        let date = file_name.strip_prefix("service-")?.strip_suffix(".log")?;
        let mut parts = date.split('-');
        let year = parts.next()?.parse::<i32>().ok()?;
        let month = parts.next()?.parse::<u32>().ok()?;
        let day = parts.next()?.parse::<u32>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        unix_day_from_date(year, month, day)
    }

    pub(super) fn utc_date_from_unix_day(day: i64) -> String {
        let (year, month, day) = date_from_unix_day(day);
        format!("{year:04}-{month:02}-{day:02}")
    }

    pub(super) fn date_from_unix_day(day: i64) -> (i32, u32, u32) {
        let z = day + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2).div_euclid(153);
        let d = doy - (153 * mp + 2).div_euclid(5) + 1;
        let m = mp + if mp < 10 { 3 } else { -9 };
        let year = y + if m <= 2 { 1 } else { 0 };
        (year as i32, m as u32, d as u32)
    }

    pub(super) fn unix_day_from_date(year: i32, month: u32, day: u32) -> Option<i64> {
        if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
            return None;
        }
        let mut y = year as i64;
        let m = month as i64;
        let d = day as i64;
        y -= if m <= 2 { 1 } else { 0 };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = m + if m > 2 { -3 } else { 9 };
        let doy = (153 * mp + 2).div_euclid(5) + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some(era * 146_097 + doe - 719_468)
    }

    pub(super) fn days_in_month(year: i32, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if is_leap_year(year) => 29,
            2 => 28,
            _ => 0,
        }
    }

    pub(super) fn is_leap_year(year: i32) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }
}
