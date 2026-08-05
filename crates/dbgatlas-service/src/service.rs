use dbgatlas_debug::{
    AddSymbolsRequest, CreateDebugSession, DebugCommandResult, DebugMemoryResult,
    DebugSessionState, DebugTarget, EvalDebugCommand, ReadMemoryRequest,
};
use dbgatlas_model::{
    ArtifactRef, DEFAULT_INLINE_RESULT_BYTE_LIMIT, Id, OperationRef, RecordingRef, ResultDelivery,
    ResultPreview, SessionRef, Timestamp,
};
use dbgatlas_recording::{
    CpuProfileSummary, CpuSamplingOptions, RecordTtd, RecordingPreset, RecordingState,
    RecordingTarget, StartRecording, TtdRecordingOptions, TtdTarget, build_ttd_args,
    ttd_stop_target,
};
use dbgatlas_runtime::{RuntimeConfig, resolve_store_windbg_dbgeng_dir};
use dbgatlas_worker_protocol::{
    ReverseCoreFunctionResult, ReverseFunctionLookupResult, WorkerArtifactRole,
    WorkerArtifactWrite, WorkerEnvelope, WorkerProtocolError, WorkerRequest, WorkerResponse,
    decode_jsonl, encode_jsonl, json_result_preview, write_atomic_bytes,
};
use dbgatlas_workspace::{
    ArtifactMetadata, CommandAuditRecord, OperationRecord, OperationStatus, Workspace,
    WorkspaceError, WorkspaceFacts,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[path = "method_registry.rs"]
mod method_registry;

use method_registry::{MethodRoute, MethodSpec, mcp_tool_descriptors, method_spec};

pub use install::{
    DoctorCheck, DoctorCheckStatus, ServiceInstallPaths, ServicePayloadFile,
    WindowsServiceApplyUpdateOptions, WindowsServiceCommandResult, WindowsServiceControlOptions,
    WindowsServiceDoctorOptions, WindowsServiceDoctorReport, WindowsServiceInstallOptions,
    WindowsServicePayloadMode, WindowsServiceRunOptions, WindowsServiceUninstallOptions,
    WindowsServiceUpdateAccepted, WindowsServiceUpdateOptions, apply_windows_service_update,
    default_windows_service_paths, default_windows_service_root, discover_service_payload,
    format_doctor_msi_summary, format_doctor_report, install_windows_service,
    installed_client_config, request_windows_service_update, run_windows_service_dispatcher,
    run_windows_service_doctor, start_windows_service, status_windows_service,
    stop_windows_service, uninstall_windows_service,
};
pub use rpc_http::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, ServiceEvent, ServiceOperation,
    ServiceOperationStatus, invoke_http_json_rpc, run_http_service, run_http_service_until,
};
pub const INTERNAL_WORKSPACE_DIR: &str = "dbgatlas";
pub const DEFAULT_SERVICE_PORT: u16 = 7331;
pub const MAX_MEMORY_READ_LENGTH: u64 = 16 * 1024 * 1024;
const MAX_WORKER_RESPONSE_LINE_BYTES: usize = 1024 * 1024;
const DEFAULT_DEBUG_SESSION_STARTUP_TIMEOUT_MS: u64 = 5_000;
const WORKER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const WORKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
pub const WINDOWS_SERVICE_NAME: &str = "DbgAtlas";
pub const WINDOWS_SERVICE_DISPLAY_NAME: &str = "DbgAtlas Service";
pub const WINDOWS_SERVICE_DESCRIPTION: &str = "DbgAtlas local debugging service";
pub const WINDOWS_SERVICE_DIR: &str = "dbgatlas";
pub const WINDOWS_SERVICE_BIN_DIR: &str = "bin";
pub const WINDOWS_SERVICE_ETC_DIR: &str = "etc";
pub const WINDOWS_SERVICE_RT_DIR: &str = "rt";
pub const WINDOWS_SERVICE_VAR_DIR: &str = "var";
pub const WINDOWS_SERVICE_LOG_DIR: &str = "log";
pub const WINDOWS_SERVICE_CONFIG_FILE: &str = "runtime.toml";
pub const WINDOWS_SERVICE_TOKEN_FILE: &str = "token";
pub const WINDOWS_SERVICE_INSTALL_MARKER_FILE: &str = ".dbgatlas-install-root";
pub const WINDOWS_SERVICE_LOG_RETENTION_DAYS: i64 = 7;
pub const WINDOWS_SERVICE_POSTINSTALL_REPORT_FILE: &str = "postinstall-report.txt";
pub const WINDOWS_SERVICE_POSTINSTALL_SUMMARY_FILE: &str = "postinstall-summary.txt";
const DBGATLAS_TOKEN_ENV_VAR: &str = "DBGATLAS_TOKEN";
pub const DEFAULT_SERVICE_UPDATE_TIMEOUT_MS: u64 = 60_000;
pub const SERVICE_UPDATE_DELAY_MS: u64 = 500;
pub const WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES: &[&str] = &[
    "dbgatlas.exe",
    "dbgatlas-worker.exe",
    "dbgatlas_dbgeng.dll",
    "dbgatlas_etw.dll",
    "dbgatlas_ida.dll",
];
pub const DEFAULT_IDA_INSTALL_DIR: &str = r"C:\Program Files\IDA Professional 9.3";
const TTD_COMMAND_HELPER_ERROR_PREFIX: &str = "dbgatlas TTD command helper error:";

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
static RECORDING_COUNTER: AtomicU64 = AtomicU64::new(1);
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);
static ARTIFACT_COUNTER: AtomicU64 = AtomicU64::new(1);
static WORKER_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);
static SERVICE_INSTALL_ROOT_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub bind: SocketAddr,
    pub bearer_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstallTokenSelection {
    token: String,
    write_system_env: bool,
}

fn resolve_install_token<F>(
    system_env_token: Option<&str>,
    file_token: Option<&str>,
    generate_token: F,
) -> Result<InstallTokenSelection, ServiceError>
where
    F: FnOnce() -> Result<String, ServiceError>,
{
    if let Some(token) = trimmed_non_empty_token(system_env_token) {
        return Ok(InstallTokenSelection {
            token,
            write_system_env: false,
        });
    }
    if let Some(token) = trimmed_non_empty_token(file_token) {
        return Ok(InstallTokenSelection {
            token,
            write_system_env: true,
        });
    }

    let token = generate_token()?;
    let token = trimmed_non_empty_token(Some(&token)).ok_or(ServiceError::EmptyBearerToken)?;
    Ok(InstallTokenSelection {
        token,
        write_system_env: true,
    })
}

fn trimmed_non_empty_token(value: Option<&str>) -> Option<String> {
    let token = value?.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

impl ServiceConfig {
    pub fn dev_default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_SERVICE_PORT),
            bearer_token: "dev-token".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct TtdCommandHelperOptions {
    pub executable: PathBuf,
    pub current_dir: Option<PathBuf>,
    pub args: Vec<OsString>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

pub fn run_ttd_command_helper(options: TtdCommandHelperOptions) -> i32 {
    match run_ttd_command_helper_inner(&options) {
        Ok(code) => code.unwrap_or(1),
        Err(error) => {
            let _ = append_ttd_helper_error(&options.stderr_path, &error);
            1
        }
    }
}

fn run_ttd_command_helper_inner(
    options: &TtdCommandHelperOptions,
) -> Result<Option<i32>, ServiceError> {
    if let Some(parent) = options.stdout_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = options.stderr_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let stdout = fs::File::create(&options.stdout_path)?;
    let stderr = fs::File::create(&options.stderr_path)?;
    let job = job::ManagedJob::create_result("DbgAtlasTtdCommandHelper")?;
    let mut command = Command::new(&options.executable);
    command
        .args(&options.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(current_dir) = &options.current_dir {
        command.current_dir(current_dir);
    }
    let mut child = command.spawn()?;
    if let Err(error) = job.assign_child_process(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ServiceError::Io(error));
    }
    Ok(child.wait()?.code())
}

fn append_ttd_helper_error(path: &Path, error: &ServiceError) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{TTD_COMMAND_HELPER_ERROR_PREFIX} {error:#}")?;
    Ok(())
}

#[derive(Clone, Default)]
pub struct ServiceShutdown {
    stop: Arc<AtomicBool>,
    active_handlers: Arc<AtomicU64>,
}

impl ServiceShutdown {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn handler_started(&self) {
        self.active_handlers.fetch_add(1, Ordering::SeqCst);
    }

    fn handler_finished(&self) {
        self.active_handlers.fetch_sub(1, Ordering::SeqCst);
    }

    fn wait_for_handlers(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        while self.active_handlers.load(Ordering::SeqCst) != 0 {
            if started.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        true
    }
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("service bind address must be loopback: {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("bearer token must not be empty")]
    EmptyBearerToken,
    #[error("missing or invalid bearer token")]
    Unauthorized,
    #[error("origin is not allowed: {0}")]
    InvalidOrigin(String),
    #[error("unsupported http method: {0}")]
    UnsupportedHttpMethod(String),
    #[error("invalid http request: {0}")]
    InvalidHttpRequest(String),
    #[error("json-rpc error: {0}")]
    Rpc(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("session is not reusable: {0}")]
    SessionNotReusable(String),
    #[error("session is already terminal: {0}")]
    SessionAlreadyTerminal(String),
    #[error("recording not found: {0}")]
    RecordingNotFound(String),
    #[error("recording is already terminal: {0}")]
    RecordingAlreadyTerminal(String),
    #[error("operation not found: {0}")]
    OperationNotFound(String),
    #[error("operation is not running and cannot be canceled: {0}")]
    OperationNotCancelable(String),
    #[error("worker error: {0}")]
    Worker(String),
    #[error("worker transport is not supported on this platform")]
    WorkerTransportUnsupported,
    #[error("Windows service control is not supported on this platform")]
    ServiceControlUnsupported,
    #[error("Windows service control error: {0}")]
    ServiceControl(String),
    #[error("service install payload is incomplete: {0}")]
    IncompleteInstallPayload(String),
    #[error("service is running; stop it before installing or updating payload")]
    ServiceIsRunning,
    #[error(transparent)]
    Debug(#[from] dbgatlas_debug::DebugError),
    #[error(transparent)]
    Recording(#[from] dbgatlas_recording::RecordingError),
    #[error(transparent)]
    Ida(#[from] dbgatlas_ida::IdaError),
    #[error(transparent)]
    Runtime(#[from] dbgatlas_runtime::RuntimeConfigError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    WorkerProtocol(#[from] WorkerProtocolError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct ServiceHost {
    state: Arc<Mutex<ServiceState>>,
    supervisor: Arc<dyn WorkerSupervisor>,
    capabilities: ServiceCapabilities,
    ttd_runner: Arc<dyn TtdRecorderRunner>,
    etw_controller: Arc<dyn EtwController>,
}

#[derive(Clone, Debug, Default)]
pub struct ServiceCapabilities {
    pub dbgeng_dirs: Vec<PathBuf>,
    pub ttd_dir: Option<PathBuf>,
}

mod host_debug;
mod host_recording;
mod host_reverse;
mod host_service;
mod host_worker_ops;

impl ServiceCapabilities {
    pub fn from_runtime_config(runtime: &RuntimeConfig) -> Self {
        let tools = runtime.resolve_tool_paths();
        Self::from_resolved_tools(tools)
    }

    pub fn from_runtime_config_with_install_paths(
        runtime: &RuntimeConfig,
        paths: &ServiceInstallPaths,
    ) -> Self {
        let tools =
            runtime.resolve_tool_paths_with_dbgatlas_windbg_runtime_dir(&paths.windbg_runtime_dir);
        Self::from_resolved_tools(tools)
    }

    fn from_resolved_tools(tools: dbgatlas_runtime::ResolvedToolPaths) -> Self {
        Self {
            dbgeng_dirs: tools
                .dbgeng_candidates
                .iter()
                .map(|location| location.dir.clone())
                .collect(),
            ttd_dir: tools.ttd.map(|location| location.dir),
        }
    }
}

#[derive(Default)]
struct ServiceState {
    sessions: HashMap<String, ManagedSession>,
    recordings: HashMap<String, ManagedRecording>,
    operations: HashMap<String, ServiceOperation>,
    active_ttd_recording: Option<RecordingRef>,
    active_cpu_recording: Option<RecordingRef>,
}

struct TtdActiveGuard {
    state: Arc<Mutex<ServiceState>>,
    recording_id: RecordingRef,
}

struct CpuActiveGuard {
    state: Arc<Mutex<ServiceState>>,
    recording_id: RecordingRef,
    committed: bool,
}

impl CpuActiveGuard {
    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CpuActiveGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.active_cpu_recording.as_ref() == Some(&self.recording_id) {
            state.active_cpu_recording = None;
        }
    }
}

impl Drop for TtdActiveGuard {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.active_ttd_recording.as_ref() == Some(&self.recording_id) {
            state.active_ttd_recording = None;
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct ManagedRecording {
    recording_id: RecordingRef,
    project_root: PathBuf,
    internal_workspace_root: PathBuf,
    artifact_dir: PathBuf,
    target: RecordingTarget,
    presets: Vec<RecordingPreset>,
    cpu_sampling: Option<CpuSamplingOptions>,
    root_pid: Option<u32>,
    state: RecordingState,
    created_at: Timestamp,
    updated_at: Timestamp,
    last_operation: Option<OperationRef>,
    artifacts: Vec<ArtifactRef>,
    #[serde(skip)]
    trace_session: Arc<Mutex<Option<Box<dyn EtwSessionControl>>>>,
    trace_start_error: Option<String>,
    trace_consumer_error: Option<String>,
    trace_stack_status: Option<dbgatlas_etw::EtwStackTraceStatus>,
    trace_stack_status_error: Option<String>,
    cpu_sampling_status: Option<dbgatlas_etw::EtwCpuSamplingStatus>,
    cpu_sampling_status_error: Option<String>,
    trace_stop_result: Option<dbgatlas_etw::EtwSessionStopResult>,
    cpu_summary: Option<CpuProfileSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct ManagedSession {
    session_id: SessionRef,
    capability: String,
    project_root: PathBuf,
    internal_workspace_root: PathBuf,
    artifact_dir: PathBuf,
    target: Option<DebugTarget>,
    database_path: Option<PathBuf>,
    ida_install_dir: Option<PathBuf>,
    state: DebugSessionState,
    worker: WorkerHandle,
    #[serde(skip)]
    request_lock: Arc<Mutex<()>>,
    created_at: Timestamp,
    updated_at: Timestamp,
    last_operation: Option<OperationRef>,
    artifacts: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, Serialize)]
struct SessionListItem {
    session_id: SessionRef,
    capability: String,
    state: DebugSessionState,
    project_root: PathBuf,
    target: Option<DebugTarget>,
    database_path: Option<PathBuf>,
    ida_install_dir: Option<PathBuf>,
    worker_id: Id,
    worker_identity: WorkerIdentity,
    created_at: Timestamp,
    updated_at: Timestamp,
    last_operation: Option<OperationRef>,
    artifact_refs: Vec<ArtifactRef>,
    close_tool: String,
    kill_tool: Option<String>,
}

impl From<&ManagedSession> for SessionListItem {
    fn from(session: &ManagedSession) -> Self {
        Self {
            session_id: session.session_id.clone(),
            capability: session.capability.clone(),
            state: session.state,
            project_root: session.project_root.clone(),
            target: session.target.clone(),
            database_path: session.database_path.clone(),
            ida_install_dir: session.ida_install_dir.clone(),
            worker_id: session.worker.worker_id.clone(),
            worker_identity: session.worker.identity.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            last_operation: session.last_operation.clone(),
            artifact_refs: session.artifacts.clone(),
            close_tool: format!("{}.session.close", session.capability),
            kill_tool: (session.capability == "debug").then(|| "debug.session.kill".to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerIdentity {
    LocalSystem,
    ActiveInteractiveUser,
    CurrentUserDevMode,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkerHandle {
    pub worker_id: Id,
    pub session_id: SessionRef,
    pub pipe_name: String,
    pub identity: WorkerIdentity,
}

#[derive(Clone, Debug)]
pub struct WorkerCreateRequest {
    pub session_id: SessionRef,
    pub project_root: PathBuf,
    pub internal_workspace_root: PathBuf,
    pub artifact_dir: PathBuf,
    pub startup_timeout_ms: u64,
    pub dbgeng_dirs: Vec<PathBuf>,
    pub identity: Option<WorkerIdentity>,
}

pub trait WorkerSupervisor: Send + Sync {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError>;
    fn request_worker(
        &self,
        worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError>;
    fn request_worker_with_timeout(
        &self,
        worker: &WorkerHandle,
        request: WorkerRequest,
        _timeout: Duration,
    ) -> Result<WorkerResponse, ServiceError> {
        self.request_worker(worker, request)
    }
    fn cancel_worker_operation(
        &self,
        worker: &WorkerHandle,
        session_id: &SessionRef,
        operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError>;
    fn close_worker(&self, worker: &WorkerHandle) -> Result<(), ServiceError>;
    fn kill_worker(&self, worker: &WorkerHandle) -> Result<(), ServiceError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerCancelOutcome {
    Notified,
    WorkerKilled,
}

#[derive(Clone, Debug)]
struct TtdRecorderInvocation {
    ttd_exe: PathBuf,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
    timeout: Duration,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    worker_identity: TtdRecorderIdentity,
    timeout_stop: Option<TtdTimeoutStop>,
}

#[derive(Clone, Debug)]
struct TtdTimeoutStop {
    stop_target: OsString,
    prefer_recorded_pid: bool,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    timeout: Duration,
    recorder_exit_timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TtdProcessExit {
    exit_code: Option<i32>,
    timed_out: bool,
    stop: Option<TtdStopExit>,
    killed_after_timeout: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TtdStopExit {
    stop_target: OsString,
    exit_code: Option<i32>,
    timed_out: bool,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TtdRecorderIdentity {
    #[default]
    Default,
    ActiveInteractiveUser,
}

trait TtdRecorderRunner: Send + Sync {
    fn run(&self, invocation: TtdRecorderInvocation) -> Result<TtdProcessExit, ServiceError>;
}

trait EtwSessionControl: Send + std::fmt::Debug {
    fn stack_trace_status(
        &self,
    ) -> Result<dbgatlas_etw::EtwStackTraceStatus, dbgatlas_etw::EtwError>;
    fn cpu_sampling_status(
        &self,
    ) -> Result<dbgatlas_etw::EtwCpuSamplingStatus, dbgatlas_etw::EtwError>;
    fn start_realtime_consumer(
        &mut self,
        events_dir: PathBuf,
        presets: dbgatlas_etw::EtwPresetFlags,
        root_pid: Option<u32>,
    ) -> Result<(), dbgatlas_etw::EtwError>;
    fn stop(self: Box<Self>) -> Result<dbgatlas_etw::EtwSessionStopResult, dbgatlas_etw::EtwError>;
}

impl EtwSessionControl for dbgatlas_etw::EtwFileSession {
    fn stack_trace_status(
        &self,
    ) -> Result<dbgatlas_etw::EtwStackTraceStatus, dbgatlas_etw::EtwError> {
        self.stack_trace_status()
    }

    fn cpu_sampling_status(
        &self,
    ) -> Result<dbgatlas_etw::EtwCpuSamplingStatus, dbgatlas_etw::EtwError> {
        self.cpu_sampling_status()
    }

    fn start_realtime_consumer(
        &mut self,
        events_dir: PathBuf,
        presets: dbgatlas_etw::EtwPresetFlags,
        root_pid: Option<u32>,
    ) -> Result<(), dbgatlas_etw::EtwError> {
        self.start_realtime_consumer(events_dir, presets, root_pid)
    }

    fn stop(self: Box<Self>) -> Result<dbgatlas_etw::EtwSessionStopResult, dbgatlas_etw::EtwError> {
        (*self).stop_with_result()
    }
}

trait EtwController: Send + Sync {
    fn start(
        &self,
        session_name: &str,
        trace_path: &Path,
        presets: dbgatlas_etw::EtwPresetFlags,
        cpu_sampling: Option<CpuSamplingOptions>,
    ) -> Result<Box<dyn EtwSessionControl>, dbgatlas_etw::EtwError>;
}

#[derive(Debug)]
struct NativeEtwController;

impl EtwController for NativeEtwController {
    fn start(
        &self,
        session_name: &str,
        trace_path: &Path,
        presets: dbgatlas_etw::EtwPresetFlags,
        cpu_sampling: Option<CpuSamplingOptions>,
    ) -> Result<Box<dyn EtwSessionControl>, dbgatlas_etw::EtwError> {
        let session = match cpu_sampling {
            Some(cpu) => dbgatlas_etw::EtwFileSession::start_with_options(
                session_name,
                trace_path,
                presets,
                dbgatlas_etw::EtwSessionOptions {
                    cpu_sampling_interval_us: cpu.interval_us,
                    maximum_trace_size_mb: cpu.max_trace_size_mb,
                },
            )?,
            None => dbgatlas_etw::EtwFileSession::start(session_name, trace_path, presets)?,
        };
        Ok(Box::new(session))
    }
}

#[derive(Debug)]
struct ProcessTtdRecorderRunner;

impl TtdRecorderRunner for ProcessTtdRecorderRunner {
    fn run(&self, invocation: TtdRecorderInvocation) -> Result<TtdProcessExit, ServiceError> {
        run_ttd_process(invocation)
    }
}

pub struct MockWorkerSupervisor {
    identity: WorkerIdentity,
}

impl MockWorkerSupervisor {
    pub fn new() -> Self {
        Self {
            identity: WorkerIdentity::CurrentUserDevMode,
        }
    }
}

impl Default for MockWorkerSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerSupervisor for MockWorkerSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        let worker_id = Id::new(format!("worker-{}", request.session_id.id.as_str()))
            .expect("generated worker ids are valid");
        Ok(WorkerHandle {
            worker_id,
            pipe_name: format!(r"\\.\pipe\dbgatlas-{}", request.session_id.id.as_str()),
            session_id: request.session_id,
            identity: request.identity.unwrap_or_else(|| self.identity.clone()),
        })
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        mock_worker_response(request)
    }

    fn cancel_worker_operation(
        &self,
        worker: &WorkerHandle,
        session_id: &SessionRef,
        operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        let _ = self.request_worker(
            worker,
            WorkerRequest::CancelOperation {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
            },
        )?;
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

pub struct ProcessWorkerSupervisor {
    identity: WorkerIdentity,
    worker_exe: Option<PathBuf>,
    workers: Mutex<HashMap<String, Arc<ProcessWorkerState>>>,
}

struct ProcessWorkerState {
    child: Mutex<WorkerProcess>,
    transport: Mutex<WorkerTransport>,
    lifetime: Mutex<WorkerLifetime>,
}

enum WorkerProcess {
    Std(Child),
    #[cfg(windows)]
    RawWindows(windows_active_user_process::RawProcess),
}

impl WorkerProcess {
    fn pid(&self) -> u32 {
        match self {
            Self::Std(child) => child.id(),
            #[cfg(windows)]
            Self::RawWindows(process) => process.pid(),
        }
    }

    fn try_wait(&mut self) -> Result<Option<Option<i32>>, std::io::Error> {
        match self {
            Self::Std(child) => child
                .try_wait()
                .map(|status| status.map(|status| status.code())),
            #[cfg(windows)]
            Self::RawWindows(process) => process.try_wait(),
        }
    }

    fn kill(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::Std(child) => child.kill(),
            #[cfg(windows)]
            Self::RawWindows(process) => process.kill(),
        }
    }

    fn wait(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::Std(child) => child.wait().map(|_| ()),
            #[cfg(windows)]
            Self::RawWindows(process) => process.wait(),
        }
    }

    fn wait_timeout(&mut self, timeout: Duration) -> Result<bool, std::io::Error> {
        let started = Instant::now();
        loop {
            if self.try_wait()?.is_some() {
                return Ok(true);
            }
            if started.elapsed() >= timeout {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

struct WorkerLifetime {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl WorkerLifetime {
    fn start(worker_id: &Id, session_id: &SessionRef, transport: WorkerTransport) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let worker_id = worker_id.as_str().to_string();
        let session_id = session_id.id.as_str().to_string();
        let join = std::thread::spawn(move || {
            let mut file = transport.into_file();
            while !thread_stop.load(Ordering::SeqCst) {
                if let Err(error) = file.write_all(b"tick\n").and_then(|_| file.flush()) {
                    append_service_diagnostic_log(&format!(
                        "worker_lifetime_heartbeat_stopped worker_id={} session_id={} error={}",
                        sanitize_log_value(&worker_id),
                        sanitize_log_value(&session_id),
                        sanitize_log_value(&error.to_string())
                    ));
                    return;
                }
                let started = Instant::now();
                while started.elapsed() < WORKER_HEARTBEAT_INTERVAL {
                    if thread_stop.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        });
        Self {
            stop,
            join: Some(join),
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if self.join.as_ref().is_some_and(|join| join.is_finished()) {
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }
}

impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
fn terminate_descendant_processes(root_pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    for _ in 0..3 {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            append_service_diagnostic_log(&format!(
                "worker_descendant_cleanup_snapshot_failed root_pid={} error={}",
                root_pid,
                sanitize_log_value(&std::io::Error::last_os_error().to_string())
            ));
            return;
        }

        let mut processes = Vec::new();
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) };
        while ok != 0 {
            processes.push((entry.th32ProcessID, entry.th32ParentProcessID));
            ok = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        unsafe {
            CloseHandle(snapshot);
        }

        let mut descendants = Vec::new();
        let mut frontier = vec![root_pid];
        while let Some(parent_pid) = frontier.pop() {
            for (pid, ppid) in &processes {
                if *ppid == parent_pid && *pid != root_pid && !descendants.contains(pid) {
                    descendants.push(*pid);
                    frontier.push(*pid);
                }
            }
        }

        if descendants.is_empty() {
            return;
        }

        for pid in descendants.into_iter().rev() {
            let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
            if handle.is_null() {
                append_service_diagnostic_log(&format!(
                    "worker_descendant_open_failed root_pid={} pid={} error={}",
                    root_pid,
                    pid,
                    sanitize_log_value(&std::io::Error::last_os_error().to_string())
                ));
                continue;
            }
            let ok = unsafe { TerminateProcess(handle, 1) };
            if ok == 0 {
                append_service_diagnostic_log(&format!(
                    "worker_descendant_terminate_failed root_pid={} pid={} error={}",
                    root_pid,
                    pid,
                    sanitize_log_value(&std::io::Error::last_os_error().to_string())
                ));
            }
            unsafe {
                CloseHandle(handle);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(windows))]
fn terminate_descendant_processes(_root_pid: u32) {}

impl ProcessWorkerSupervisor {
    pub fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            identity: WorkerIdentity::CurrentUserDevMode,
            worker_exe: None,
            workers: Mutex::new(HashMap::new()),
        })
    }

    pub fn new_with_worker_exe(worker_exe: PathBuf) -> Result<Self, ServiceError> {
        Ok(Self {
            identity: WorkerIdentity::CurrentUserDevMode,
            worker_exe: Some(worker_exe),
            workers: Mutex::new(HashMap::new()),
        })
    }

    pub fn new_installed_service() -> Result<Self, ServiceError> {
        Ok(Self {
            identity: WorkerIdentity::LocalSystem,
            worker_exe: None,
            workers: Mutex::new(HashMap::new()),
        })
    }

    fn get_worker(&self, worker: &WorkerHandle) -> Result<Arc<ProcessWorkerState>, ServiceError> {
        let workers = self
            .workers
            .lock()
            .map_err(|_| ServiceError::Worker("worker registry lock poisoned".to_string()))?;
        workers
            .get(worker.worker_id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::Worker(format!("worker not found: {}", worker.worker_id)))
    }
}

impl WorkerSupervisor for ProcessWorkerSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        let started = Instant::now();
        let worker_id = Id::new(format!("worker-{}", request.session_id.id.as_str()))
            .expect("generated worker ids are valid");
        let pipe_name = unique_pipe_name(&request.session_id);
        let lifetime_pipe_name = unique_lifetime_pipe_name(&request.session_id);
        let identity = request.identity.unwrap_or_else(|| self.identity.clone());
        let dbgeng_dirs = prepare_dbgeng_dirs_for_worker_identity(&identity, &request.dbgeng_dirs)?;
        let dbgeng_dir_count = dbgeng_dirs.len();
        append_service_diagnostic_log(&format!(
            "worker_create_start worker_id={} session_id={} identity={} startup_timeout_ms={} dbgeng_dir_count={}",
            sanitize_log_value(worker_id.as_str()),
            sanitize_log_value(request.session_id.id.as_str()),
            sanitize_log_value(&format!("{:?}", identity)),
            request.startup_timeout_ms,
            dbgeng_dir_count,
        ));
        let transport = WorkerTransport::create_server(&pipe_name).map_err(|error| {
            append_worker_create_failed_log(
                &worker_id,
                &request.session_id,
                &identity,
                started,
                "pipe_server",
                &error.to_string(),
            );
            ServiceError::Worker(format!("failed to create worker pipe server: {error}"))
        })?;
        let lifetime_transport =
            WorkerTransport::create_server(&lifetime_pipe_name).map_err(|error| {
                append_worker_create_failed_log(
                    &worker_id,
                    &request.session_id,
                    &identity,
                    started,
                    "lifetime_pipe_server",
                    &error.to_string(),
                );
                ServiceError::Worker(format!(
                    "failed to create worker lifetime pipe server: {error}"
                ))
            })?;
        let worker_exe = self
            .worker_exe
            .clone()
            .map(Ok)
            .unwrap_or_else(worker_executable_path)
            .map_err(|error| {
                append_worker_create_failed_log(
                    &worker_id,
                    &request.session_id,
                    &identity,
                    started,
                    "worker_executable",
                    &error.to_string(),
                );
                ServiceError::Worker(format!("failed to resolve worker executable path: {error}"))
            })?;
        let mut child = spawn_worker_process(
            &worker_exe,
            &pipe_name,
            &lifetime_pipe_name,
            request.session_id.id.as_str(),
            &identity,
            &dbgeng_dirs,
        )
        .inspect_err(|error| {
            append_worker_create_failed_log(
                &worker_id,
                &request.session_id,
                &identity,
                started,
                "spawn",
                &error.to_string(),
            );
        })?;
        let connected = match transport.connect(request.startup_timeout_ms) {
            Ok(connected) => connected,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                append_worker_create_failed_log(
                    &worker_id,
                    &request.session_id,
                    &identity,
                    started,
                    "connect",
                    &error.to_string(),
                );
                return Err(error);
            }
        };
        let lifetime_connected = match lifetime_transport.connect(request.startup_timeout_ms) {
            Ok(connected) => connected,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                append_worker_create_failed_log(
                    &worker_id,
                    &request.session_id,
                    &identity,
                    started,
                    "lifetime_connect",
                    &error.to_string(),
                );
                return Err(error);
            }
        };
        let lifetime = WorkerLifetime::start(&worker_id, &request.session_id, lifetime_connected);
        let state = Arc::new(ProcessWorkerState {
            child: Mutex::new(child),
            transport: Mutex::new(connected),
            lifetime: Mutex::new(lifetime),
        });
        self.workers
            .lock()
            .map_err(|_| ServiceError::Worker("worker registry lock poisoned".to_string()))?
            .insert(worker_id.as_str().to_string(), state);
        append_service_diagnostic_log(&format!(
            "worker_create_complete worker_id={} session_id={} identity={} duration_ms={} dbgeng_dir_count={}",
            sanitize_log_value(worker_id.as_str()),
            sanitize_log_value(request.session_id.id.as_str()),
            sanitize_log_value(&format!("{:?}", identity)),
            started.elapsed().as_millis(),
            dbgeng_dir_count,
        ));
        Ok(WorkerHandle {
            worker_id,
            pipe_name,
            session_id: request.session_id,
            identity,
        })
    }

    fn request_worker(
        &self,
        worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        let worker_state = self.get_worker(worker)?;
        let mut transport = worker_state
            .transport
            .lock()
            .map_err(|_| ServiceError::Worker("worker transport lock poisoned".to_string()))?;
        transport.request(worker, request)
    }

    fn request_worker_with_timeout(
        &self,
        worker: &WorkerHandle,
        request: WorkerRequest,
        timeout: Duration,
    ) -> Result<WorkerResponse, ServiceError> {
        let worker_state = self.get_worker(worker)?;
        let started = Instant::now();
        loop {
            match worker_state.transport.try_lock() {
                Ok(mut transport) => {
                    return transport.request_with_timeout(worker, request, timeout);
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(ServiceError::Worker(
                        "worker transport lock poisoned".to_string(),
                    ));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if started.elapsed() >= timeout {
                        return Err(ServiceError::Worker(format!(
                            "timed out waiting for worker transport lock after {} ms",
                            timeout.as_millis()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }

    fn cancel_worker_operation(
        &self,
        worker: &WorkerHandle,
        session_id: &SessionRef,
        operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        let worker_state = self.get_worker(worker)?;
        if let Ok(mut transport) = worker_state.transport.try_lock() {
            let _ = transport.request(
                worker,
                WorkerRequest::CancelOperation {
                    session_id: session_id.clone(),
                    operation_id: operation_id.clone(),
                },
            )?;
            append_service_diagnostic_log(&format!(
                "worker_cancel_notified worker_id={} session_id={} operation_id={}",
                sanitize_log_value(worker.worker_id.as_str()),
                sanitize_log_value(session_id.id.as_str()),
                sanitize_log_value(operation_id.id.as_str())
            ));
            return Ok(WorkerCancelOutcome::Notified);
        }
        // worker pipe 同一时间只能服务一个请求；拿不到锁说明目标请求仍在执行。
        // 这时继续排队 cancel 已经无法及时生效，只能终止 worker 保证上层 operation 进入终态。
        self.kill_worker(worker)?;
        append_service_diagnostic_log(&format!(
            "worker_cancel_killed worker_id={} session_id={} operation_id={}",
            sanitize_log_value(worker.worker_id.as_str()),
            sanitize_log_value(session_id.id.as_str()),
            sanitize_log_value(operation_id.id.as_str())
        ));
        Ok(WorkerCancelOutcome::WorkerKilled)
    }

    fn close_worker(&self, worker: &WorkerHandle) -> Result<(), ServiceError> {
        let started = Instant::now();
        let worker_state = self
            .workers
            .lock()
            .map_err(|_| ServiceError::Worker("worker registry lock poisoned".to_string()))?
            .remove(worker.worker_id.as_str());
        let found = worker_state.is_some();
        if let Some(worker_state) = worker_state {
            if let Ok(mut lifetime) = worker_state.lifetime.lock() {
                lifetime.stop();
            }
            let mut child = worker_state
                .child
                .lock()
                .map_err(|_| ServiceError::Worker("worker process lock poisoned".to_string()))?;
            if !child.wait_timeout(WORKER_SHUTDOWN_TIMEOUT)? {
                append_service_diagnostic_log(&format!(
                    "worker_close_timeout worker_id={} session_id={} identity={} timeout_ms={}",
                    sanitize_log_value(worker.worker_id.as_str()),
                    sanitize_log_value(worker.session_id.id.as_str()),
                    sanitize_log_value(&format!("{:?}", worker.identity)),
                    WORKER_SHUTDOWN_TIMEOUT.as_millis(),
                ));
                terminate_descendant_processes(child.pid());
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        append_service_diagnostic_log(&format!(
            "worker_close worker_id={} session_id={} identity={} found={} duration_ms={}",
            sanitize_log_value(worker.worker_id.as_str()),
            sanitize_log_value(worker.session_id.id.as_str()),
            sanitize_log_value(&format!("{:?}", worker.identity)),
            found,
            started.elapsed().as_millis(),
        ));
        Ok(())
    }

    fn kill_worker(&self, worker: &WorkerHandle) -> Result<(), ServiceError> {
        let started = Instant::now();
        let worker_state = self
            .workers
            .lock()
            .map_err(|_| ServiceError::Worker("worker registry lock poisoned".to_string()))?
            .remove(worker.worker_id.as_str());
        let found = worker_state.is_some();
        if let Some(worker_state) = worker_state {
            if let Ok(mut lifetime) = worker_state.lifetime.lock() {
                lifetime.stop();
            }
            let mut child = worker_state
                .child
                .lock()
                .map_err(|_| ServiceError::Worker("worker process lock poisoned".to_string()))?;
            terminate_descendant_processes(child.pid());
            let _ = child.kill();
            let _ = child.wait();
        }
        append_service_diagnostic_log(&format!(
            "worker_kill worker_id={} session_id={} identity={} found={} duration_ms={}",
            sanitize_log_value(worker.worker_id.as_str()),
            sanitize_log_value(worker.session_id.id.as_str()),
            sanitize_log_value(&format!("{:?}", worker.identity)),
            found,
            started.elapsed().as_millis(),
        ));
        Ok(())
    }
}

impl Drop for ProcessWorkerSupervisor {
    fn drop(&mut self) {
        if let Ok(mut workers) = self.workers.lock() {
            for (_, worker) in workers.drain() {
                if let Ok(mut lifetime) = worker.lifetime.lock() {
                    lifetime.stop();
                }
                if let Ok(mut child) = worker.child.lock() {
                    terminate_descendant_processes(child.pid());
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }
}

fn append_worker_create_failed_log(
    worker_id: &Id,
    session_id: &SessionRef,
    identity: &WorkerIdentity,
    started: Instant,
    stage: &str,
    error: &str,
) {
    append_service_diagnostic_log(&format!(
        "worker_create_failed worker_id={} session_id={} identity={} stage={} duration_ms={} error={}",
        sanitize_log_value(worker_id.as_str()),
        sanitize_log_value(session_id.id.as_str()),
        sanitize_log_value(&format!("{:?}", identity)),
        sanitize_log_value(stage),
        started.elapsed().as_millis(),
        sanitize_log_value(error)
    ));
}

mod recording_support;
use recording_support::*;

mod reverse_support;
use reverse_support::*;

mod mock_worker;
use mock_worker::*;

mod worker_artifacts;
use worker_artifacts::*;

mod worker_process;
use worker_process::*;

mod install;
use install::*;

mod worker_transport;
use worker_transport::*;

mod rpc_http;
use rpc_http::*;

mod workspace_query;
use workspace_query::*;

mod service_util;
use service_util::*;

mod platform;
use platform::*;

#[cfg(test)]
mod tests;
