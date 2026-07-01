use dbgatlas_debug::{
    AddSymbolsRequest, CreateDebugSession, DEFAULT_INLINE_TEXT_BYTE_LIMIT, DebugCommandResult,
    DebugMemoryResult, DebugSessionState, DebugTarget, EvalDebugCommand, ReadMemoryRequest,
    inline_text_preview,
};
use dbgatlas_model::{ArtifactRef, Id, OperationRef, RecordingRef, SessionRef, Timestamp};
use dbgatlas_recording::{
    RecordTtd, RecordingPreset, RecordingState, RecordingTarget, StartRecording,
    TtdRecordingOptions, TtdTarget, build_ttd_args, ttd_stop_target,
};
use dbgatlas_runtime::{RuntimeConfig, resolve_store_windbg_dbgeng_dir};
use dbgatlas_worker_protocol::{
    ReverseCoreFunctionResult, ReverseFunctionLookupResult, WorkerArtifactWrite, WorkerEnvelope,
    WorkerProtocolError, WorkerRequest, WorkerResponse, decode_jsonl, encode_jsonl,
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
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub const INTERNAL_WORKSPACE_DIR: &str = "dbgatlas";
pub const DEFAULT_SERVICE_PORT: u16 = 7331;
pub const MAX_MEMORY_READ_LENGTH: u64 = 16 * 1024 * 1024;
const MAX_INLINE_REVERSE_RESULT_BYTES: usize = 128 * 1024;
const MAX_INLINE_DECOMPILE_PSEUDOCODE_BYTES: usize = 32 * 1024;
const MAX_WORKER_RESPONSE_LINE_BYTES: usize = 1024 * 1024;
const DEFAULT_DEBUG_SESSION_STARTUP_TIMEOUT_MS: u64 = 5_000;
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
    let mut child = Command::new(&options.executable)
        .args(&options.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()?;
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
}

#[derive(Clone, Debug, Default)]
pub struct ServiceCapabilities {
    pub ida_py_eval: bool,
    pub dbgeng_dirs: Vec<PathBuf>,
    pub ttd_dir: Option<PathBuf>,
}

impl ServiceHost {
    pub fn new(supervisor: Arc<dyn WorkerSupervisor>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ServiceState::default())),
            supervisor,
            capabilities: ServiceCapabilities::from_runtime_config(&RuntimeConfig::default()),
            ttd_runner: Arc::new(ProcessTtdRecorderRunner),
        }
    }

    pub fn with_mock_workers() -> Self {
        Self::new(Arc::new(MockWorkerSupervisor::new()))
    }

    pub fn with_process_workers() -> Result<Self, ServiceError> {
        Ok(Self::new(Arc::new(ProcessWorkerSupervisor::new()?)))
    }

    pub fn with_process_worker_exe(worker_exe: PathBuf) -> Result<Self, ServiceError> {
        Ok(Self::new(Arc::new(
            ProcessWorkerSupervisor::new_with_worker_exe(worker_exe)?,
        )))
    }

    pub fn with_installed_process_workers() -> Result<Self, ServiceError> {
        Ok(Self::new(Arc::new(
            ProcessWorkerSupervisor::new_installed_service()?,
        )))
    }

    pub fn with_ida_py_eval(mut self, enabled: bool) -> Self {
        self.capabilities.ida_py_eval = enabled;
        self
    }

    pub fn with_capabilities(mut self, capabilities: ServiceCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    #[cfg(test)]
    fn with_ttd_runner(mut self, runner: Arc<dyn TtdRecorderRunner>) -> Self {
        self.ttd_runner = runner;
        self
    }

    pub fn handle_rpc(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();
        let result = match request.method.as_str() {
            "service.health" => self.service_health(),
            "service.info" => self.service_info(),
            "service.update" => self.service_update(request.params),
            "operation.get" => self.operation_get(request.params),
            "operation.cancel" => self.operation_cancel(request.params),
            "operation.stream" => self.operation_stream(request.params),
            "debug.session.create" => self.debug_session_create(request.params),
            "debug.session.close" => self.debug_session_close(request.params),
            "debug.session.kill" => self.debug_session_kill(request.params),
            "debug.eval" => self.debug_eval(request.params),
            "debug.eval_steps" => self.debug_eval_steps(request.params),
            "debug.modules" => self.debug_builtin_eval(request.params, "debug.modules", "lm"),
            "debug.threads" => self.debug_builtin_eval(request.params, "debug.threads", "~"),
            "debug.stack" => self.debug_builtin_eval(request.params, "debug.stack", "k"),
            "debug.add_symbols" => self.debug_add_symbols(request.params),
            "debug.read_memory" => self.debug_read_memory(request.params),
            "reverse.session.open" => self.reverse_session_open(request.params),
            "reverse.lookup_function" => self.reverse_lookup_function(request.params),
            "reverse.lookup_funcs" => self.reverse_core_function("lookup_funcs", request.params),
            "reverse.int_convert" => self.reverse_core_function("int_convert", request.params),
            "reverse.list_funcs" => self.reverse_core_function("list_funcs", request.params),
            "reverse.list_globals" => self.reverse_core_function("list_globals", request.params),
            "reverse.list_imports" => self.reverse_core_function("list_imports", request.params),
            "reverse.list_strings" => self.reverse_core_function("list_strings", request.params),
            "reverse.get_string" => self.reverse_core_function("get_string", request.params),
            "reverse.get_bytes" => self.reverse_core_function("get_bytes", request.params),
            "reverse.get_int" => self.reverse_core_function("get_int", request.params),
            "reverse.decompile" => self.reverse_core_function("decompile", request.params),
            "reverse.disasm" => self.reverse_core_function("disasm", request.params),
            "reverse.xrefs_to" => self.reverse_core_function("xrefs_to", request.params),
            "reverse.xrefs_to_field" => {
                self.reverse_core_function("xrefs_to_field", request.params)
            }
            "reverse.callees" => self.reverse_core_function("callees", request.params),
            "reverse.rename" => self.reverse_core_function("rename", request.params),
            "reverse.set_comments" => self.reverse_core_function("set_comments", request.params),
            "reverse.set_type" => self.reverse_core_function("set_type", request.params),
            "reverse.declare_type" => self.reverse_core_function("declare_type", request.params),
            "reverse.inspect_item" => self.reverse_core_function("inspect_item", request.params),
            "reverse.force_recompile" => {
                self.reverse_core_function("force_recompile", request.params)
            }
            "reverse.idb_save" => self.reverse_core_function("idb_save", request.params),
            "reverse.py_eval" => self.reverse_py_eval(request.params),
            "reverse.find_bytes" => self.reverse_core_function("find_bytes", request.params),
            "reverse.search_text" => self.reverse_core_function("search_text", request.params),
            "reverse.query_xrefs" => self.reverse_core_function("query_xrefs", request.params),
            "reverse.query_funcs" => self.reverse_core_function("query_funcs", request.params),
            "reverse.query_entities" => {
                self.reverse_core_function("query_entities", request.params)
            }
            "reverse.session.close" => self.reverse_session_close(request.params),
            "recording.start" => self.recording_start(request.params),
            "recording.ttd" => self.recording_ttd(request.params),
            "recording.status" => self.recording_status(request.params),
            "recording.stop" => self.recording_stop(request.params),
            "recording.cancel" => self.recording_cancel(request.params),
            "recording.kill" => self.recording_kill(request.params),
            other => Err(ServiceError::Rpc(format!("unknown method `{other}`"))),
        };

        match result {
            Ok(value) => JsonRpcResponse::result(id, value),
            Err(error) => JsonRpcResponse::error(id, rpc_error_for(error)),
        }
    }

    pub fn handle_mcp(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let context = DiagnosticContext::from_rpc(&request);
        if request.id.is_none() {
            if let Err(error) = self.handle_mcp_method(request) {
                append_service_diagnostic_log(&format!(
                    "mcp_dispatch_error {} error={}",
                    context.log_fields(),
                    sanitize_log_value(&error.to_string())
                ));
            }
            return None;
        }
        let id = request.id.clone();
        Some(match self.handle_mcp_method(request) {
            Ok(result) => JsonRpcResponse::result(id, result),
            Err(error) => {
                append_service_diagnostic_log(&format!(
                    "mcp_dispatch_error {} error={}",
                    context.log_fields(),
                    sanitize_log_value(&error.to_string())
                ));
                JsonRpcResponse::error(id, mcp_error_for(error))
            }
        })
    }

    fn handle_mcp_method(&self, request: JsonRpcRequest) -> Result<Value, ServiceError> {
        match request.method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "dbgatlas-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "tools": {},
                },
            })),
            "ping" => Ok(json!({})),
            "notifications/initialized" => Ok(json!(null)),
            "tools/list" => Ok(json!({ "tools": mcp_tool_descriptors(self.capabilities.clone()) })),
            "tools/call" => {
                let params: ToolCallParams =
                    serde_json::from_value(request.params.unwrap_or_else(|| json!({})))?;
                let arguments = params.arguments.unwrap_or_else(|| json!({}));
                let result = self.call_mcp_tool_output(&params.name, arguments.clone())?;
                if result.is_error {
                    append_mcp_tool_error_log(&params.name, &arguments, &result);
                }
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result.value)?,
                    }],
                    "isError": result.is_error,
                }))
            }
            other => Err(ServiceError::Rpc(format!("unknown MCP method `{other}`"))),
        }
    }

    fn call_mcp_tool_output(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<ToolCallOutput, ServiceError> {
        match name {
            "service.health"
            | "service.info"
            | "service.update"
            | "operation.get"
            | "operation.cancel"
            | "operation.stream"
            | "debug.session.create"
            | "debug.session.close"
            | "debug.session.kill"
            | "debug.eval"
            | "debug.eval_steps"
            | "debug.modules"
            | "debug.threads"
            | "debug.stack"
            | "debug.add_symbols"
            | "debug.read_memory"
            | "reverse.session.open"
            | "reverse.lookup_function"
            | "reverse.lookup_funcs"
            | "reverse.int_convert"
            | "reverse.list_funcs"
            | "reverse.list_globals"
            | "reverse.list_imports"
            | "reverse.list_strings"
            | "reverse.get_string"
            | "reverse.get_bytes"
            | "reverse.get_int"
            | "reverse.decompile"
            | "reverse.disasm"
            | "reverse.xrefs_to"
            | "reverse.xrefs_to_field"
            | "reverse.callees"
            | "reverse.rename"
            | "reverse.set_comments"
            | "reverse.set_type"
            | "reverse.declare_type"
            | "reverse.inspect_item"
            | "reverse.force_recompile"
            | "reverse.idb_save"
            | "reverse.find_bytes"
            | "reverse.search_text"
            | "reverse.query_xrefs"
            | "reverse.query_funcs"
            | "reverse.query_entities"
            | "reverse.session.close"
            | "recording.ttd" => self.call_mcp_service_tool(name, arguments),
            "reverse.py_eval" => {
                self.ensure_ida_py_eval_enabled()?;
                self.call_mcp_service_tool(name, arguments)
            }
            "workspace.facts" => Ok(ToolCallOutput::success(
                self.mcp_workspace_facts(arguments)?,
            )),
            other => Err(ServiceError::Rpc(format!(
                "unknown DbgAtlas tool `{other}`"
            ))),
        }
    }

    fn call_mcp_service_tool(
        &self,
        method: &str,
        arguments: Value,
    ) -> Result<ToolCallOutput, ServiceError> {
        if !arguments.is_object() {
            return Err(ServiceError::Rpc(
                "tool arguments must be a JSON object".to_string(),
            ));
        }
        let response = self.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params: Some(arguments.clone()),
        });
        let mut output = mcp_service_response_result(response);
        if let Some(operation_id) = output
            .operation_id
            .clone()
            .or_else(|| self.latest_operation_id_from_arguments(&arguments))
        {
            output.set_operation_id(operation_id);
        }
        Ok(output)
    }

    fn reverse_py_eval(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.ensure_ida_py_eval_enabled()?;
        self.reverse_core_function("py_eval", params)
    }

    fn ensure_ida_py_eval_enabled(&self) -> Result<(), ServiceError> {
        if self.capabilities.ida_py_eval {
            Ok(())
        } else {
            Err(ServiceError::Rpc(
                "reverse.py_eval is disabled by runtime policy".to_string(),
            ))
        }
    }

    fn mcp_workspace_facts(&self, arguments: Value) -> Result<Value, ServiceError> {
        let params: WorkspaceFactsParams = serde_json::from_value(arguments)?;
        Ok(serde_json::to_value(workspace_facts_with_fallback(
            &params.path,
        )?)?)
    }

    fn service_health(&self) -> Result<Value, ServiceError> {
        Ok(json!({
            "status": "ok",
            "service": "DbgAtlas",
            "version": env!("CARGO_PKG_VERSION"),
        }))
    }

    fn service_info(&self) -> Result<Value, ServiceError> {
        let state = self.lock_state()?;
        Ok(json!({
            "service": "DbgAtlas",
            "version": env!("CARGO_PKG_VERSION"),
            "session_count": state.sessions.len(),
            "operation_count": state.operations.len(),
            "external_api": "json-rpc-2.0-over-http",
        }))
    }

    fn service_update(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: ServiceUpdateParams = parse_params(params)?;
        let accepted = request_windows_service_update(WindowsServiceUpdateOptions {
            source_dir: params.source_dir,
            restart: params.restart,
            timeout_ms: params.timeout_ms,
        })?;
        Ok(serde_json::to_value(accepted)?)
    }

    fn operation_get(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: OperationGetParams = parse_params(params)?;
        let state = self.lock_state()?;
        let operation = state
            .operations
            .get(params.operation_id.id.as_str())
            .ok_or_else(|| ServiceError::OperationNotFound(params.operation_id.to_string()))?;
        Ok(serde_json::to_value(operation)?)
    }

    fn operation_cancel(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: OperationGetParams = parse_params(params)?;
        let (operation, session) = {
            let mut state = self.lock_state()?;
            let operation = state
                .operations
                .get_mut(params.operation_id.id.as_str())
                .ok_or_else(|| ServiceError::OperationNotFound(params.operation_id.to_string()))?;
            if operation.status != ServiceOperationStatus::Running {
                return Err(ServiceError::OperationNotCancelable(
                    params.operation_id.to_string(),
                ));
            }
            operation.status = ServiceOperationStatus::Canceled;
            operation.summary = "operation canceled".to_string();
            operation.updated_at = Timestamp::now();
            operation.events.push(ServiceEvent {
                timestamp: Timestamp::now(),
                kind: "canceled".to_string(),
                message: "operation canceled by request".to_string(),
            });
            let operation = operation.clone();
            let session = operation
                .session_id
                .as_ref()
                .and_then(|session_id| state.sessions.get(session_id.id.as_str()).cloned());
            (operation, session)
        };

        if let Some(session) = session {
            let cancel_outcome = self.supervisor.cancel_worker_operation(
                &session.worker,
                &session.session_id,
                &params.operation_id,
            )?;
            if cancel_outcome == WorkerCancelOutcome::WorkerKilled {
                let mut state = self.lock_state()?;
                if let Some(session) = state.sessions.get_mut(session.session_id.id.as_str()) {
                    session.state = DebugSessionState::Error;
                    session.updated_at = Timestamp::now();
                }
            }
            let workspace = Workspace::open(&session.internal_workspace_root)?;
            workspace.append_operation(&OperationRecord {
                operation_id: params.operation_id,
                adapter_id: "service".to_string(),
                capability: operation.capability.clone(),
                status: OperationStatus::Canceled,
                created_at: Timestamp::now(),
                summary: "operation canceled".to_string(),
                artifacts: operation.artifacts.clone(),
                raw_output: operation.raw_output.clone(),
            })?;
        }

        Ok(serde_json::to_value(operation)?)
    }

    fn operation_stream(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: OperationGetParams = parse_params(params)?;
        let state = self.lock_state()?;
        let operation = state
            .operations
            .get(params.operation_id.id.as_str())
            .ok_or_else(|| ServiceError::OperationNotFound(params.operation_id.to_string()))?;
        Ok(json!({
            "operation_id": operation.operation_id,
            "events": operation.events,
        }))
    }

    fn recording_start(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: RecordingStartParams = parse_params(params)?;
        let request = StartRecording {
            target: params.target,
            presets: params.presets,
        }
        .validate()?;
        let recording_id = next_recording_ref();
        let operation_id = next_operation_ref();
        let workspace = ensure_project_workspace(&params.project_root)?;
        let artifact_dir = workspace.ensure_recording_artifact_dir(&recording_id.id)?;
        let now = Timestamp::now();
        let trace_path = artifact_dir.join("trace.etl");
        let trace_session_name = etw_session_name(&recording_id);
        let trace_preset_flags = etw_preset_flags(&request.presets);
        let (mut trace_session, trace_start_error) = match dbgatlas_etw::EtwFileSession::start(
            &trace_session_name,
            &trace_path,
            trace_preset_flags,
        ) {
            Ok(session) => (Some(session), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let (trace_stack_status, trace_stack_status_error) = match trace_session.as_ref() {
            Some(session) => match session.stack_trace_status() {
                Ok(status) => (Some(status), None),
                Err(error) => (None, Some(error.to_string())),
            },
            None => (None, None),
        };
        let prepared_target = match prepare_recording_target(&request.target) {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Some(session) = trace_session {
                    let _ = session.stop();
                }
                return Err(error);
            }
        };
        let root_pid = Some(prepared_target.root_pid());
        let trace_consumer_error = match trace_session.as_mut() {
            Some(session) => match session.start_realtime_consumer(
                artifact_dir.join("events"),
                trace_preset_flags,
                root_pid,
            ) {
                Ok(()) => None,
                Err(error) => Some(error.to_string()),
            },
            None => None,
        };
        if let Err(error) = prepared_target.resume() {
            if let Some(session) = trace_session {
                let _ = session.stop();
            }
            return Err(error);
        }
        let trace_session = Arc::new(Mutex::new(trace_session));
        let writes = write_recording_start_artifacts(
            &artifact_dir,
            &recording_id,
            &operation_id,
            &request,
            root_pid,
            now,
            trace_start_error.as_deref(),
            trace_consumer_error.as_deref(),
            trace_stack_status,
            trace_stack_status_error.as_deref(),
        )?;
        let registered = register_worker_writes(&workspace, &operation_id, &writes)?;

        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "recording.start".to_string(),
            status: OperationStatus::Success,
            created_at: now,
            summary: "recording started".to_string(),
            artifacts: registered.artifacts.clone(),
            raw_output: registered.raw_output.clone(),
        })?;

        let recording = ManagedRecording {
            recording_id: recording_id.clone(),
            project_root: params.project_root,
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir,
            target: request.target,
            presets: request.presets,
            root_pid,
            state: RecordingState::Running,
            created_at: now,
            updated_at: now,
            last_operation: Some(operation_id.clone()),
            artifacts: registered.artifacts.clone(),
            trace_session,
            trace_start_error,
            trace_consumer_error,
            trace_stack_status,
            trace_stack_status_error,
        };

        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "recording.start",
            None,
            "recording started",
        );
        operation.artifacts = registered.artifacts.clone();
        operation.raw_output = registered.raw_output.clone();

        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        state
            .recordings
            .insert(recording_id.id.as_str().to_string(), recording.clone());

        Ok(recording_response(
            &recording,
            &operation_id,
            OperationStatus::Success,
            &registered,
        )?)
    }

    fn recording_ttd(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: RecordingTtdParams = parse_params(params)?;
        let worker_identity = params.worker_identity;
        let request = RecordTtd {
            target: params.target,
            timeout_ms: params.timeout_ms,
            options: params.options,
        }
        .validate()?;
        let recording_id = next_recording_ref();
        let operation_id = next_operation_ref();
        let _active_guard = self.start_ttd_recording(recording_id.clone())?;
        let workspace = ensure_project_workspace(&params.project_root)?;
        let artifact_dir = workspace.ensure_recording_artifact_dir(&recording_id.id)?;
        let traces_dir = artifact_dir.join("traces");
        fs::create_dir_all(&traces_dir)?;

        let ttd_exe = self.resolve_ttd_exe()?;
        let started = Instant::now();
        let started_at = Timestamp::now();
        let stdout_path = artifact_dir.join("recorder.stdout.txt");
        let stderr_path = artifact_dir.join("recorder.stderr.txt");
        let stop_stdout_path = artifact_dir.join("recorder-stop.stdout.txt");
        let stop_stderr_path = artifact_dir.join("recorder-stop.stderr.txt");
        let events_path = artifact_dir.join("events.jsonl");
        append_ttd_recording_event(
            &events_path,
            &recording_id,
            &operation_id,
            "ttd_recording_started",
            started_at,
            json!({
                "target": request.target,
                "timeout_ms": request.timeout_ms,
                "worker_identity": worker_identity,
                "ttd_exe": ttd_exe,
            }),
            None,
        )?;

        let args = build_ttd_args(&request.target, &request.options, &traces_dir);
        append_ttd_recording_event(
            &events_path,
            &recording_id,
            &operation_id,
            "recorder_starting",
            Timestamp::now(),
            json!({
                "args": args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>()
            }),
            None,
        )?;

        let timeout_stop_target = ttd_stop_target(&request.target, None);
        let recorder_exit = self.ttd_runner.run(TtdRecorderInvocation {
            ttd_exe: ttd_exe.clone(),
            args,
            timeout: Duration::from_millis(request.timeout_ms),
            stdout_path: stdout_path.clone(),
            stderr_path: stderr_path.clone(),
            worker_identity,
            timeout_stop: timeout_stop_target
                .clone()
                .map(|stop_target| TtdTimeoutStop {
                    stop_target,
                    prefer_recorded_pid: matches!(&request.target, TtdTarget::Launch { .. }),
                    stdout_path: stop_stdout_path.clone(),
                    stderr_path: stop_stderr_path.clone(),
                    timeout: Duration::from_secs(15),
                    recorder_exit_timeout: Duration::from_secs(15),
                }),
        });
        ensure_file_exists(&stdout_path)?;
        ensure_file_exists(&stderr_path)?;

        let mut warnings = Vec::new();
        let mut error = None;
        let mut recorder_exit_code = None;
        let mut timed_out = false;
        let mut recorder_stop = None;
        let mut recorder_killed_after_timeout = false;
        match recorder_exit {
            Ok(exit) => {
                recorder_exit_code = exit.exit_code;
                timed_out = exit.timed_out;
                recorder_stop = exit.stop;
                recorder_killed_after_timeout = exit.killed_after_timeout;
                append_ttd_recording_event(
                    &events_path,
                    &recording_id,
                    &operation_id,
                    "recorder_finished",
                    Timestamp::now(),
                    json!({
                        "exit_code": exit.exit_code,
                        "timed_out": exit.timed_out,
                    }),
                    None,
                )?;
            }
            Err(run_error) => {
                error = Some(format!("start TTD recorder failed: {run_error}"));
                append_ttd_recording_event(
                    &events_path,
                    &recording_id,
                    &operation_id,
                    "recorder_failed",
                    Timestamp::now(),
                    json!({}),
                    error.clone(),
                )?;
            }
        }

        let target_pid = parse_first_recorded_pid(
            &(read_text_lossy(&stdout_path) + "\n" + read_text_lossy(&stderr_path).as_str()),
        );
        let mut stop_stdout_registered = false;
        let mut stop_stderr_registered = false;
        if timed_out {
            if recorder_killed_after_timeout {
                warnings
                    .push("TTD recorder did not exit after stop and was terminated".to_string());
            }
            append_ttd_recording_event(
                &events_path,
                &recording_id,
                &operation_id,
                "timeout_reached",
                Timestamp::now(),
                json!({}),
                None,
            )?;
            if let Some(stop) = recorder_stop {
                ensure_file_exists(&stop_stdout_path)?;
                ensure_file_exists(&stop_stderr_path)?;
                stop_stdout_registered = true;
                stop_stderr_registered = true;
                if let Some(stop_error) = stop.error {
                    let warning = format!("TTD stop failed after timeout: {stop_error}");
                    warnings.push(warning.clone());
                    append_ttd_recording_event(
                        &events_path,
                        &recording_id,
                        &operation_id,
                        "recorder_stop_failed",
                        Timestamp::now(),
                        json!({ "stop_target": stop.stop_target.to_string_lossy() }),
                        Some(warning),
                    )?;
                } else {
                    if stop.timed_out {
                        warnings.push("TTD stop command timed out".to_string());
                    }
                    append_ttd_recording_event(
                        &events_path,
                        &recording_id,
                        &operation_id,
                        "recorder_stop_finished",
                        Timestamp::now(),
                        json!({
                            "stop_target": stop.stop_target.to_string_lossy(),
                            "exit_code": stop.exit_code,
                            "timed_out": stop.timed_out,
                        }),
                        None,
                    )?;
                }
            } else {
                let message = match timeout_stop_target {
                    Some(stop_target) => format!(
                        "TTD recorder timed out, but stop was not attempted for {}",
                        stop_target.to_string_lossy()
                    ),
                    None => {
                        "TTD recorder timed out, but DbgAtlas could not determine a stop target"
                            .to_string()
                    }
                };
                warnings.push(message);
            }
        }

        let discovered = discover_ttd_artifacts(&traces_dir)?;
        let status = if error.is_some() {
            "failed"
        } else if recorder_exit_code.is_some_and(|code| code != 0) {
            error = Some(recorder_error_summary(
                &stdout_path,
                &stderr_path,
                recorder_exit_code,
            ));
            "failed"
        } else if discovered.traces.is_empty() {
            error = Some("TTD recorder completed but no .run trace was created".to_string());
            "failed"
        } else if timed_out {
            "timed_out"
        } else {
            "completed"
        };
        let operation_status = if status == "failed" {
            OperationStatus::Failed
        } else {
            OperationStatus::Success
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        let stopped_at = Timestamp::now();
        let metadata = ttd_recording_metadata_json(TtdRecordingMetadata {
            recording_id: &recording_id,
            operation_id: &operation_id,
            request: &request,
            status,
            operation_status: &operation_status,
            target_pid,
            recorder_exit_code,
            timed_out,
            started_at,
            stopped_at,
            duration_ms,
            ttd_exe: &ttd_exe,
            worker_identity,
            discovered: &discovered,
            warnings: &warnings,
            error: error.as_deref(),
        });
        let metadata_len = write_json_file(&artifact_dir.join("recording.json"), &metadata)?;
        append_ttd_recording_event(
            &events_path,
            &recording_id,
            &operation_id,
            "recording_completed",
            stopped_at,
            json!({
                "status": status,
                "operation_status": operation_status,
                "trace_count": discovered.traces.len(),
                "duration_ms": duration_ms,
            }),
            error.clone(),
        )?;

        let writes = ttd_recording_writes(
            &recording_id,
            &artifact_dir,
            metadata_len,
            stop_stdout_registered,
            stop_stderr_registered,
            &discovered,
        )?;
        let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
        let trace_artifacts = ttd_trace_artifact_results(&workspace, &writes, &registered);
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "recording.ttd".to_string(),
            status: operation_status.clone(),
            created_at: started_at,
            summary: format!("TTD recording {status}"),
            artifacts: registered.artifacts.clone(),
            raw_output: registered.raw_output.clone(),
        })?;

        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "recording.ttd",
            None,
            format!("TTD recording {status}"),
        );
        if operation_status == OperationStatus::Failed {
            operation.status = ServiceOperationStatus::Failed;
        }
        operation.artifacts = registered.artifacts.clone();
        operation.raw_output = registered.raw_output.clone();
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);

        Ok(json!({
            "recording_id": recording_id,
            "state": status,
            "target": request.target,
            "mode": request.target.mode(),
            "worker_identity": worker_identity,
            "operation_id": operation_id,
            "operation_status": operation_status,
            "target_pid": target_pid,
            "recorder_exit_code": recorder_exit_code,
            "duration_ms": duration_ms,
            "primary_trace_path": discovered.traces.first(),
            "trace_paths": discovered.traces,
            "trace_index_paths": discovered.trace_indexes,
            "trace_artifacts": trace_artifacts,
            "artifact_refs": registered.artifacts,
            "raw_output_ref": registered.raw_output,
            "warnings": warnings,
            "error": error,
            "operation": {
                "status": operation_status,
                "artifact_refs": registered.artifacts,
                "raw_output_ref": registered.raw_output,
            }
        }))
    }

    fn recording_status(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: RecordingParams = parse_params(params)?;
        let state = self.lock_state()?;
        let recording = state
            .recordings
            .get(params.recording_id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::RecordingNotFound(params.recording_id.to_string()))?;
        Ok(recording_status_response(&recording))
    }

    fn recording_stop(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_recording(params, RecordingFinishMode::Stop)
    }

    fn recording_cancel(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_recording(params, RecordingFinishMode::Cancel)
    }

    fn recording_kill(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_recording(params, RecordingFinishMode::Kill)
    }

    fn finish_recording(
        &self,
        params: Option<Value>,
        mode: RecordingFinishMode,
    ) -> Result<Value, ServiceError> {
        let params: RecordingParams = parse_params(params)?;
        let mut recording = {
            let mut state = self.lock_state()?;
            let recording = state
                .recordings
                .get_mut(params.recording_id.id.as_str())
                .ok_or_else(|| ServiceError::RecordingNotFound(params.recording_id.to_string()))?;
            if recording.state != RecordingState::Running {
                return Err(ServiceError::RecordingAlreadyTerminal(
                    recording.recording_id.to_string(),
                ));
            }
            recording.state = RecordingState::Stopping;
            recording.updated_at = Timestamp::now();
            recording.clone()
        };

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&recording.internal_workspace_root)?;
        let now = Timestamp::now();
        let (state, status, capability, summary, writes) = match mode {
            RecordingFinishMode::Stop => {
                let writes = write_recording_stop_artifacts(
                    &recording.artifact_dir,
                    &recording,
                    &operation_id,
                    now,
                )?;
                (
                    RecordingState::Stopped,
                    OperationStatus::Success,
                    "recording.stop",
                    "recording stopped",
                    writes,
                )
            }
            RecordingFinishMode::Cancel => (
                RecordingState::Canceled,
                OperationStatus::Canceled,
                "recording.cancel",
                "recording canceled",
                write_recording_terminal_trace_artifacts(
                    &recording.artifact_dir,
                    &recording,
                    &operation_id,
                    now,
                    "canceled",
                )?,
            ),
            RecordingFinishMode::Kill => (
                RecordingState::Killed,
                OperationStatus::Failed,
                "recording.kill",
                "recording killed",
                write_recording_terminal_trace_artifacts(
                    &recording.artifact_dir,
                    &recording,
                    &operation_id,
                    now,
                    "killed",
                )?,
            ),
        };
        let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: capability.to_string(),
            status: status.clone(),
            created_at: now,
            summary: summary.to_string(),
            artifacts: registered.artifacts.clone(),
            raw_output: registered.raw_output.clone(),
        })?;

        recording.state = state;
        recording.updated_at = now;
        recording.last_operation = Some(operation_id.clone());
        recording.artifacts = registered.artifacts.clone();

        let mut operation =
            ServiceOperation::success(operation_id.clone(), capability, None, summary);
        if status == OperationStatus::Canceled {
            operation.status = ServiceOperationStatus::Canceled;
        }
        if status == OperationStatus::Failed {
            operation.status = ServiceOperationStatus::Failed;
        }
        operation.artifacts = registered.artifacts.clone();
        operation.raw_output = registered.raw_output.clone();

        let mut state = self.lock_state()?;
        state.recordings.insert(
            recording.recording_id.id.as_str().to_string(),
            recording.clone(),
        );
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);

        Ok(recording_response(
            &recording,
            &operation_id,
            status,
            &registered,
        )?)
    }

    fn debug_session_create(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params = parse_debug_session_create_params(params)?;
        let target = params.target.clone().validate()?;
        let request = CreateDebugSession {
            target: target.clone(),
            startup_timeout_ms: params.startup_timeout_ms,
        }
        .validate()?;
        let session_id = next_session_ref();
        let operation_id = next_operation_ref();
        let workspace = ensure_project_workspace(&params.project_root)?;
        let session_dir = workspace.ensure_session_artifact_dir(&session_id.id)?;
        let mut start = self.try_start_debug_worker(
            &params,
            &request,
            &session_id,
            &operation_id,
            &workspace,
            &session_dir,
            params.worker_identity.worker_create_identity(),
            params.worker_identity,
        );
        if start.worker.is_none()
            && should_auto_retry_active_interactive_user(
                params.worker_identity,
                &request.target,
                &start.failures,
            )
        {
            append_service_diagnostic_log(&format!(
                "debug_session_create_auto_retry_active_user session_id={} operation_id={} target_kind={}",
                sanitize_log_value(session_id.id.as_str()),
                sanitize_log_value(operation_id.id.as_str()),
                sanitize_log_value(debug_target_kind(&request.target))
            ));
            let retry = self.try_start_debug_worker(
                &params,
                &request,
                &session_id,
                &operation_id,
                &workspace,
                &session_dir,
                Some(WorkerIdentity::ActiveInteractiveUser),
                DebugWorkerIdentity::ActiveInteractiveUser,
            );
            if retry.worker.is_some() {
                start = retry;
            } else {
                start.failures.extend(retry.failures);
            }
        }
        let Some(worker) = start.worker else {
            let message = start.failures.join(" | ");
            self.record_failed_session_create(&workspace, &operation_id, &session_id, &message)?;
            append_service_diagnostic_log(&format!(
                "debug_session_create_failed session_id={} operation_id={} error={}",
                sanitize_log_value(session_id.id.as_str()),
                sanitize_log_value(operation_id.id.as_str()),
                sanitize_log_value(&message)
            ));
            return Err(ServiceError::Worker(message));
        };
        let start_writes = start
            .writes
            .expect("worker start writes are set with started worker");
        let now = Timestamp::now();
        let registered_start_writes =
            register_worker_writes(&workspace, &operation_id, &start_writes)?;

        let session = ManagedSession {
            session_id: session_id.clone(),
            capability: "debug".to_string(),
            project_root: params.project_root,
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: session_dir,
            target: Some(target),
            database_path: None,
            ida_install_dir: None,
            state: DebugSessionState::Ready,
            worker,
            request_lock: Arc::new(Mutex::new(())),
            created_at: now,
            updated_at: now,
            last_operation: Some(operation_id.clone()),
            artifacts: registered_start_writes.artifacts.clone(),
        };

        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "debug.session.create",
            Some(session_id.clone()),
            "debug session created",
        );
        operation.artifacts = registered_start_writes.artifacts.clone();
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "debug.session.create".to_string(),
            status: OperationStatus::Success,
            created_at: now,
            summary: "debug session created".to_string(),
            artifacts: registered_start_writes.artifacts.clone(),
            raw_output: registered_start_writes.raw_output.clone(),
        })?;

        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        state
            .sessions
            .insert(session_id.id.as_str().to_string(), session.clone());

        append_service_diagnostic_log(&format!(
            "debug_session_create_complete session_id={} operation_id={} artifact_count={}",
            sanitize_log_value(session_id.id.as_str()),
            sanitize_log_value(operation_id.id.as_str()),
            registered_start_writes.artifacts.len()
        ));

        Ok(json!({
            "session_id": session_id,
            "state": session.state,
            "operation_id": operation_id,
            "operation_status": "success",
            "artifact_refs": registered_start_writes.artifacts,
            "raw_output_ref": registered_start_writes.raw_output,
            "operation": {
                "status": "success",
                "artifact_refs": registered_start_writes.artifacts,
                "raw_output_ref": registered_start_writes.raw_output,
            },
        }))
    }

    fn debug_session_close(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_session(params, SessionFinishMode::Close)
    }

    fn debug_session_kill(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_session(params, SessionFinishMode::Kill)
    }

    fn finish_session(
        &self,
        params: Option<Value>,
        mode: SessionFinishMode,
    ) -> Result<Value, ServiceError> {
        let params: SessionParams = parse_params(params)?;
        let mut session = {
            let state = self.lock_state()?;
            state
                .sessions
                .get(params.session_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(params.session_id.to_string()))?
        };
        if session.state.is_terminal() {
            return Err(ServiceError::SessionAlreadyTerminal(
                params.session_id.to_string(),
            ));
        }
        if session.capability != "debug" {
            return Err(ServiceError::Rpc(format!(
                "session {} is {}, expected debug",
                params.session_id, session.capability
            )));
        }
        let request_lock = session.request_lock.clone();
        let _request_guard = match mode {
            SessionFinishMode::Close => Some(
                request_lock
                    .lock()
                    .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?,
            ),
            SessionFinishMode::Kill => None,
        };
        session = {
            let state = self.lock_state()?;
            state
                .sessions
                .get(params.session_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(params.session_id.to_string()))?
        };
        if session.state.is_terminal() {
            return Err(ServiceError::SessionAlreadyTerminal(
                params.session_id.to_string(),
            ));
        }
        if session.capability != "debug" {
            return Err(ServiceError::Rpc(format!(
                "session {} is {}, expected debug",
                params.session_id, session.capability
            )));
        }
        let operation_id = next_operation_ref();
        match mode {
            SessionFinishMode::Close => {
                let close_response = self.supervisor.request_worker(
                    &session.worker,
                    WorkerRequest::CloseSession {
                        session_id: session.session_id.clone(),
                    },
                );
                match close_response {
                    Err(error) => {
                        let cleanup = self.supervisor.kill_worker(&session.worker);
                        self.mark_debug_session_error(&session.session_id)?;
                        let cleanup_message = match cleanup {
                            Ok(()) => "worker was killed after close failure".to_string(),
                            Err(cleanup_error) => format!(
                                "worker cleanup also failed: {cleanup_error}; use debug.session.kill"
                            ),
                        };
                        return Err(ServiceError::Worker(format!(
                            "debug.session.close could not complete cooperatively: {error}; {cleanup_message}"
                        )));
                    }
                    Ok(WorkerResponse::Ok { .. }) => {
                        self.supervisor.close_worker(&session.worker)?
                    }
                    Ok(WorkerResponse::Failed { code, message, .. }) => {
                        return Err(ServiceError::Worker(format!(
                            "{code}: {message}; use debug.session.kill if the worker is stuck"
                        )));
                    }
                    Ok(other) => {
                        return Err(ServiceError::Worker(format!(
                            "unexpected close response: {other:?}; use debug.session.kill if the worker is stuck"
                        )));
                    }
                }
            }
            SessionFinishMode::Kill => self.supervisor.kill_worker(&session.worker)?,
        }
        session.state = DebugSessionState::Closed;
        session.updated_at = Timestamp::now();
        session.last_operation = Some(operation_id.clone());
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let capability = match mode {
            SessionFinishMode::Close => "debug.session.close",
            SessionFinishMode::Kill => "debug.session.kill",
        };
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: capability.to_string(),
            status: OperationStatus::Success,
            created_at: Timestamp::now(),
            summary: format!("{capability} complete"),
            artifacts: Vec::new(),
            raw_output: None,
        })?;

        let operation = ServiceOperation::success(
            operation_id.clone(),
            capability,
            Some(session.session_id.clone()),
            format!("{capability} complete"),
        );
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        state
            .sessions
            .insert(session.session_id.id.as_str().to_string(), session.clone());
        Ok(json!({
            "session_id": session.session_id,
            "state": session.state,
            "operation_id": operation_id,
            "operation_status": "success",
            "artifact_refs": [],
            "raw_output_ref": null,
            "operation": {
                "status": "success",
                "artifact_refs": [],
                "raw_output_ref": null,
            },
        }))
    }

    fn try_start_debug_worker(
        &self,
        params: &DebugSessionCreateParams,
        request: &CreateDebugSession,
        session_id: &SessionRef,
        operation_id: &OperationRef,
        workspace: &Workspace,
        session_dir: &Path,
        worker_identity: Option<WorkerIdentity>,
        failure_identity: DebugWorkerIdentity,
    ) -> DebugSessionStartAttempt {
        let mut started_worker = None;
        let mut start_writes = None;
        let mut start_failures = Vec::new();
        let dbgeng_attempts =
            debug_worker_dbgeng_attempts(&request.target, &self.capabilities.dbgeng_dirs);
        append_service_diagnostic_log(&format!(
            "debug_session_create_start session_id={} operation_id={} target_kind={} identity={} attempt_count={} workspace={}",
            sanitize_log_value(session_id.id.as_str()),
            sanitize_log_value(operation_id.id.as_str()),
            sanitize_log_value(debug_target_kind(&request.target)),
            sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
            dbgeng_attempts.len(),
            sanitize_log_value(&workspace.root().display().to_string())
        ));
        for (attempt_index, dbgeng_dirs) in dbgeng_attempts.into_iter().enumerate() {
            append_service_diagnostic_log(&format!(
                "debug_session_create_attempt session_id={} operation_id={} identity={} attempt={} dbgeng_dir_count={}",
                sanitize_log_value(session_id.id.as_str()),
                sanitize_log_value(operation_id.id.as_str()),
                sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                attempt_index + 1,
                dbgeng_dirs.len()
            ));
            let worker = match self.supervisor.create_worker(WorkerCreateRequest {
                session_id: session_id.clone(),
                project_root: params.project_root.clone(),
                internal_workspace_root: workspace.root().to_path_buf(),
                artifact_dir: session_dir.to_path_buf(),
                startup_timeout_ms: request
                    .startup_timeout_ms
                    .unwrap_or(DEFAULT_DEBUG_SESSION_STARTUP_TIMEOUT_MS),
                dbgeng_dirs,
                identity: worker_identity.clone(),
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    let message = add_debug_access_denied_hint(error.to_string(), failure_identity);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_error session_id={} operation_id={} identity={} attempt={} error={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1,
                        sanitize_log_value(&message)
                    ));
                    start_failures.push(message);
                    break;
                }
            };
            let start = self.supervisor.request_worker(
                &worker,
                WorkerRequest::StartDebugSession {
                    session_id: session_id.clone(),
                    target: request.target.clone(),
                    artifact_dir: session_dir.to_path_buf(),
                },
            );
            match start {
                Ok(WorkerResponse::Ok { writes, .. }) => {
                    started_worker = Some(worker);
                    start_writes = Some(writes);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_ok session_id={} operation_id={} identity={} attempt={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1
                    ));
                    break;
                }
                Ok(WorkerResponse::Failed { code, message, .. }) => {
                    let _ = self.supervisor.kill_worker(&worker);
                    let failure = debug_start_failure_message(code, message, failure_identity);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_failed session_id={} operation_id={} identity={} attempt={} code={} error={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1,
                        sanitize_log_value(failure.code.as_str()),
                        sanitize_log_value(failure.message.as_str())
                    ));
                    start_failures.push(failure.to_string());
                }
                Ok(other) => {
                    let _ = self.supervisor.kill_worker(&worker);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_unexpected session_id={} operation_id={} identity={} attempt={} response={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1,
                        sanitize_log_value(&format!("{other:?}"))
                    ));
                    start_failures.push(format!("unexpected start response: {other:?}"));
                    break;
                }
                Err(error) => {
                    let _ = self.supervisor.kill_worker(&worker);
                    let message = add_debug_access_denied_hint(error.to_string(), failure_identity);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_error session_id={} operation_id={} identity={} attempt={} error={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1,
                        sanitize_log_value(&message)
                    ));
                    start_failures.push(message);
                    break;
                }
            }
        }

        DebugSessionStartAttempt {
            worker: started_worker,
            writes: start_writes,
            failures: start_failures,
        }
    }

    fn debug_eval(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugEvalParams = parse_params(params)?;
        validate_optional_timeout_ms(params.timeout_ms)?;
        let request = EvalDebugCommand {
            session_id: params.session_id.clone(),
            command: params.command,
            timeout_ms: params.timeout_ms,
        };
        request.validate()?;
        self.eval_command(request, "debug.eval")
    }

    fn debug_eval_steps(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugEvalStepsParams = parse_params(params)?;
        validate_optional_timeout_ms(params.timeout_ms)?;
        if params.commands.is_empty() {
            return Err(ServiceError::Rpc(
                "debug.eval_steps commands must not be empty".to_string(),
            ));
        }
        for command in &params.commands {
            EvalDebugCommand {
                session_id: params.session_id.clone(),
                command: command.clone(),
                timeout_ms: params.timeout_ms,
            }
            .validate()?;
        }

        let session = self.reusable_debug_session(&params.session_id)?;
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = self.reusable_debug_session(&params.session_id)?;
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let batch_operation_id = next_operation_ref();
        let now = Timestamp::now();
        {
            let mut state = self.lock_state()?;
            state.operations.insert(
                batch_operation_id.id.as_str().to_string(),
                ServiceOperation::running(
                    batch_operation_id.clone(),
                    "debug.eval_steps",
                    Some(session.session_id.clone()),
                    "debug.eval_steps running",
                ),
            );
        }

        let mut steps = Vec::new();
        let mut artifact_refs = Vec::new();
        let mut raw_output_ref = None;
        let mut failed_step_index = None;
        let mut canceled_step_index = None;
        for (index, command) in params.commands.iter().enumerate() {
            let step_operation_id = next_operation_ref();
            {
                let mut state = self.lock_state()?;
                state.operations.insert(
                    step_operation_id.id.as_str().to_string(),
                    ServiceOperation::running(
                        step_operation_id.clone(),
                        "debug.eval_steps.step",
                        Some(session.session_id.clone()),
                        "debug.eval_steps step running",
                    ),
                );
            }

            let worker_response = self.request_worker_with_optional_timeout(
                &session,
                WorkerRequest::EvalDebugCommand {
                    session_id: session.session_id.clone(),
                    operation_id: step_operation_id.clone(),
                    command: command.clone(),
                    artifact_dir: session.artifact_dir.clone(),
                },
                &step_operation_id,
                params.timeout_ms,
            );
            let step_result = self.finish_command_worker_response(
                &session,
                &workspace,
                step_operation_id.clone(),
                "debug.eval_steps.step",
                command.clone(),
                worker_response,
            );
            match step_result {
                Ok(mut value) => {
                    if let Some(object) = value.as_object_mut() {
                        object.insert("step_index".to_string(), json!(index));
                    }
                    let state = self.lock_state()?;
                    if let Some(operation) = state.operations.get(step_operation_id.id.as_str()) {
                        if operation.status == ServiceOperationStatus::Canceled
                            && canceled_step_index.is_none()
                        {
                            canceled_step_index = Some(index);
                        }
                        artifact_refs.extend(operation.artifacts.clone());
                        raw_output_ref = operation.raw_output.clone().or(raw_output_ref);
                    }
                    steps.push(value);
                }
                Err(error) => {
                    let state = self.lock_state()?;
                    let (step_status, step_artifacts, step_raw_output) = state
                        .operations
                        .get(step_operation_id.id.as_str())
                        .map(|operation| {
                            (
                                operation.status.clone(),
                                operation.artifacts.clone(),
                                operation.raw_output.clone(),
                            )
                        })
                        .unwrap_or((ServiceOperationStatus::Running, Vec::new(), None));
                    drop(state);
                    if step_status == ServiceOperationStatus::Running {
                        self.finish_operation_in_memory(
                            &step_operation_id,
                            ServiceOperationStatus::Failed,
                            error.to_string(),
                            Vec::new(),
                            None,
                        )?;
                        self.finish_operation_in_memory(
                            &batch_operation_id,
                            ServiceOperationStatus::Failed,
                            error.to_string(),
                            artifact_refs.clone(),
                            raw_output_ref.clone(),
                        )?;
                        return Err(error);
                    }
                    artifact_refs.extend(step_artifacts);
                    raw_output_ref = step_raw_output.or(raw_output_ref);
                    let operation_status = match step_status {
                        ServiceOperationStatus::Canceled => {
                            if canceled_step_index.is_none() {
                                canceled_step_index = Some(index);
                            }
                            "canceled"
                        }
                        _ => {
                            if failed_step_index.is_none() {
                                failed_step_index = Some(index);
                            }
                            "failed"
                        }
                    };
                    if operation_status == "canceled" && !params.continue_on_error {
                        steps.push(json!({
                            "step_index": index,
                            "session_id": session.session_id,
                            "operation_id": step_operation_id,
                            "operation_status": operation_status,
                            "command": command,
                            "error": error.to_string(),
                        }));
                        break;
                    }
                    steps.push(json!({
                        "step_index": index,
                        "session_id": session.session_id,
                        "operation_id": step_operation_id,
                        "operation_status": operation_status,
                        "command": command,
                        "error": error.to_string(),
                    }));
                    if !params.continue_on_error {
                        break;
                    }
                }
            }

            let current_session = self.reusable_debug_session(&session.session_id);
            if current_session.is_err() {
                if failed_step_index.is_none() {
                    failed_step_index = Some(index);
                }
                break;
            }
        }

        let batch_was_canceled =
            self.operation_status(&batch_operation_id)? == Some(ServiceOperationStatus::Canceled);
        let status = if batch_was_canceled || canceled_step_index.is_some() {
            OperationStatus::Canceled
        } else if failed_step_index.is_some() {
            OperationStatus::Failed
        } else {
            OperationStatus::Success
        };
        let summary = match (canceled_step_index, failed_step_index) {
            (Some(index), _) => format!("debug.eval_steps canceled at step {index}"),
            (_, Some(index)) => format!("debug.eval_steps failed at step {index}"),
            _ if batch_was_canceled => "debug.eval_steps canceled".to_string(),
            _ => "debug.eval_steps completed".to_string(),
        };
        workspace.append_operation(&OperationRecord {
            operation_id: batch_operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "debug.eval_steps".to_string(),
            status: status.clone(),
            created_at: now,
            summary: summary.clone(),
            artifacts: artifact_refs.clone(),
            raw_output: raw_output_ref.clone(),
        })?;
        workspace.append_command_audit(&CommandAuditRecord {
            operation_id: batch_operation_id.clone(),
            session_id: Some(session.session_id.clone()),
            capability: "debug.eval_steps".to_string(),
            command: params.commands.join("\n"),
            created_at: now,
            status: status.clone(),
            artifacts: artifact_refs.clone(),
            raw_output: raw_output_ref.clone(),
        })?;

        let service_status = match status {
            OperationStatus::Failed => ServiceOperationStatus::Failed,
            OperationStatus::Canceled => ServiceOperationStatus::Canceled,
            _ => ServiceOperationStatus::Success,
        };
        self.finish_operation_in_memory(
            &batch_operation_id,
            service_status,
            summary,
            artifact_refs.clone(),
            raw_output_ref.clone(),
        )?;
        {
            let mut state = self.lock_state()?;
            if let Some(session_state) = state.sessions.get_mut(session.session_id.id.as_str()) {
                session_state.last_operation = Some(batch_operation_id.clone());
                session_state.updated_at = Timestamp::now();
            }
        }

        Ok(json!({
            "session_id": session.session_id,
            "operation_id": batch_operation_id,
            "operation_status": status,
            "failed_step_index": failed_step_index,
            "steps": steps,
            "artifact_refs": artifact_refs,
            "raw_output_ref": raw_output_ref,
            "operation": {
                "status": status,
                "artifact_refs": artifact_refs,
                "raw_output_ref": raw_output_ref,
            },
        }))
    }

    fn debug_builtin_eval(
        &self,
        params: Option<Value>,
        capability: &'static str,
        command: &'static str,
    ) -> Result<Value, ServiceError> {
        let params: SessionParams = parse_params(params)?;
        let request = EvalDebugCommand {
            session_id: params.session_id,
            command: command.to_string(),
            timeout_ms: None,
        };
        self.eval_command(request, capability)
    }

    fn debug_add_symbols(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugAddSymbolsParams = parse_params(params)?;
        let request = AddSymbolsRequest {
            session_id: params.session_id,
            symbol_path: params.symbol_path,
            reload: params.reload,
        };
        request.validate()?;
        self.add_symbols(request)
    }

    fn debug_read_memory(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugReadMemoryParams = parse_params(params)?;
        let request = ReadMemoryRequest {
            session_id: params.session_id,
            address: parse_u64_param(&params.address, "address")?,
            length: params.length,
        };
        request.validate(MAX_MEMORY_READ_LENGTH)?;
        self.read_memory(request)
    }

    fn reverse_session_open(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: ReverseSessionOpenParams = parse_params(params)?;
        let session_id = next_session_ref();
        let operation_id = next_operation_ref();
        let workspace = ensure_project_workspace(&params.project_root)?;
        let artifact_dir = workspace.ensure_reverse_session_artifact_dir(&session_id.id)?;
        let ida_install_dir = params
            .ida_install_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_IDA_INSTALL_DIR));
        let worker = match self.supervisor.create_worker(WorkerCreateRequest {
            session_id: session_id.clone(),
            project_root: params.project_root.clone(),
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: artifact_dir.clone(),
            startup_timeout_ms: 5_000,
            dbgeng_dirs: self.capabilities.dbgeng_dirs.clone(),
            identity: Some(WorkerIdentity::ActiveInteractiveUser),
        }) {
            Ok(worker) => worker,
            Err(error) => {
                let message = diagnose_reverse_open_error(format!(
                    "failed to create reverse worker: {error}"
                ));
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &session_id,
                    &artifact_dir,
                    &operation_id,
                    "reverse.session.open",
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.open".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(message));
            }
        };

        let open = self.supervisor.request_worker(
            &worker,
            WorkerRequest::OpenReverseSession {
                session_id: session_id.clone(),
                ida_install_dir: ida_install_dir.clone(),
                database_path: params.database_path.clone(),
            },
        );
        match open {
            Ok(WorkerResponse::ReverseSessionOpened { .. }) => {}
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let _ = self.supervisor.kill_worker(&worker);
                let error = diagnose_reverse_open_error(worker_failed_message(code, message));
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &session_id,
                    &artifact_dir,
                    &operation_id,
                    "reverse.session.open",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.open".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Ok(other) => {
                let _ = self.supervisor.kill_worker(&worker);
                let error = diagnose_reverse_open_error(format!(
                    "unexpected reverse open response: {other:?}"
                ));
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &session_id,
                    &artifact_dir,
                    &operation_id,
                    "reverse.session.open",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.open".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Err(error) => {
                let _ = self.supervisor.kill_worker(&worker);
                let message = diagnose_reverse_open_error(error.to_string());
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &session_id,
                    &artifact_dir,
                    &operation_id,
                    "reverse.session.open",
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.open".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message,
                    artifacts,
                    None,
                )?;
                return Err(error);
            }
        };

        let result = (|| {
            let now = Timestamp::now();
            let metadata = json!({
                "session_id": session_id,
                "database_path": params.database_path,
                "ida_install_dir": ida_install_dir,
                "created_at": now,
                "mode": "native_dynamic_idalib",
                "writes_idb": false,
                "open_operation_writes_idb": false,
                "session_write_capable": true
            });
            let session_metadata_file = "sessions/session.json";
            let byte_len = write_json_file(&artifact_dir.join(&session_metadata_file), &metadata)?;
            let artifact_id = next_artifact_ref();
            workspace.register_artifact(&ArtifactMetadata {
                artifact_id: artifact_id.clone(),
                kind: "reverse.session".to_string(),
                relative_path: reverse_relative_path(&session_id, session_metadata_file),
                created_at: now,
                operation_id: Some(operation_id.clone()),
                byte_len: Some(byte_len),
                description: Some("IDA reverse session metadata".to_string()),
            })?;
            workspace.append_operation(&OperationRecord {
                operation_id: operation_id.clone(),
                adapter_id: "ida".to_string(),
                capability: "reverse.session.open".to_string(),
                status: OperationStatus::Success,
                created_at: now,
                summary: "reverse session opened".to_string(),
                artifacts: vec![artifact_id.clone()],
                raw_output: None,
            })?;

            let session = ManagedSession {
                session_id: session_id.clone(),
                capability: "reverse".to_string(),
                project_root: params.project_root,
                internal_workspace_root: workspace.root().to_path_buf(),
                artifact_dir,
                target: None,
                database_path: Some(params.database_path),
                ida_install_dir: Some(ida_install_dir),
                state: DebugSessionState::Ready,
                worker: worker.clone(),
                request_lock: Arc::new(Mutex::new(())),
                created_at: now,
                updated_at: now,
                last_operation: Some(operation_id.clone()),
                artifacts: vec![artifact_id.clone()],
            };

            let mut operation = ServiceOperation::success(
                operation_id.clone(),
                "reverse.session.open",
                Some(session_id.clone()),
                "reverse session opened",
            );
            operation.artifacts = vec![artifact_id.clone()];
            let mut state = self.lock_state()?;
            state
                .operations
                .insert(operation_id.id.as_str().to_string(), operation);
            state
                .sessions
                .insert(session_id.id.as_str().to_string(), session.clone());

            Ok(json!({
                "session_id": session_id,
                "operation_id": operation_id,
                "operation_status": "success",
                "artifact_refs": [artifact_id],
                "operation": {
                    "status": "success",
                    "artifact_refs": [artifact_id],
                    "raw_output_ref": null
                }
            }))
        })();
        if result.is_err() {
            let close = self
                .supervisor
                .request_worker(&worker, WorkerRequest::CloseReverseSession { session_id });
            if matches!(close, Ok(WorkerResponse::Ok { .. })) {
                let _ = self.supervisor.close_worker(&worker);
            } else {
                let _ = self.supervisor.kill_worker(&worker);
            }
        }
        result
    }

    fn reverse_lookup_function(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: ReverseLookupFunctionParams = parse_params(params)?;
        let runtime_address = parse_u64_param(&params.runtime_address, "runtime_address")?;
        let runtime_module_base =
            parse_u64_param(&params.runtime_module_base, "runtime_module_base")?;
        let ida_image_base = parse_u64_param(&params.ida_image_base, "ida_image_base")?;
        let session = self.reusable_reverse_session(&params.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let lookup = {
            let _request_guard = session
                .request_lock
                .lock()
                .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
            self.supervisor.request_worker(
                &session.worker,
                WorkerRequest::LookupReverseFunction {
                    session_id: params.session_id.clone(),
                    runtime_address,
                    runtime_module_base,
                    ida_image_base,
                },
            )
        };
        let lookup = match lookup {
            Ok(WorkerResponse::ReverseFunctionLookup { result, .. }) => result,
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let error = worker_failed_message(code, message);
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.lookup_function",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.lookup_function".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Ok(other) => {
                let error = format!("unexpected reverse lookup response: {other:?}");
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.lookup_function",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.lookup_function".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Err(error) => {
                let message = error.to_string();
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.lookup_function",
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.lookup_function".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message,
                    artifacts,
                    None,
                )?;
                return Err(error);
            }
        };
        let now = Timestamp::now();
        let event = json!({
            "operation_id": operation_id,
            "session_id": params.session_id,
            "created_at": now,
            "lookup": lookup,
        });
        let lookup_file = format!("lookups/{}.jsonl", operation_id.id.as_str());
        let byte_len = write_jsonl_file(&session.artifact_dir.join(&lookup_file), &event)?;
        let artifact_id = next_artifact_ref();
        workspace.register_artifact(&ArtifactMetadata {
            artifact_id: artifact_id.clone(),
            kind: "reverse.lookup".to_string(),
            relative_path: reverse_relative_path(&params.session_id, &lookup_file),
            created_at: now,
            operation_id: Some(operation_id.clone()),
            byte_len: Some(byte_len),
            description: Some("IDA function lookup result".to_string()),
        })?;
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "ida".to_string(),
            capability: "reverse.lookup_function".to_string(),
            status: OperationStatus::Success,
            created_at: now,
            summary: if lookup.found {
                "IDA function located".to_string()
            } else {
                "IDA function not found".to_string()
            },
            artifacts: vec![artifact_id.clone()],
            raw_output: None,
        })?;

        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "reverse.lookup_function",
            Some(params.session_id.clone()),
            "reverse lookup completed",
        );
        operation.artifacts = vec![artifact_id.clone()];
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        if let Some(session) = state.sessions.get_mut(params.session_id.id.as_str()) {
            session.updated_at = now;
            session.last_operation = Some(operation_id.clone());
            session.artifacts.push(artifact_id.clone());
        }

        Ok(json!({
            "session_id": params.session_id,
            "operation_id": operation_id,
            "operation_status": "success",
            "artifact_refs": [artifact_id],
            "runtime_address": lookup.runtime_address,
            "runtime_module_base": lookup.runtime_module_base,
            "rva": lookup.rva,
            "ida_image_base": lookup.ida_image_base,
            "ida_ea": lookup.ida_ea,
            "function_start": lookup.function_start,
            "function_end": lookup.function_end,
            "function_name": lookup.function_name,
            "found": lookup.found,
            "operation": {
                "status": "success",
                "artifact_refs": [artifact_id],
                "raw_output_ref": null
            }
        }))
    }

    fn reverse_core_function(
        &self,
        function: &'static str,
        params: Option<Value>,
    ) -> Result<Value, ServiceError> {
        let params: ReverseCoreFunctionParams = parse_params(params)?;
        let session = self.reusable_reverse_session(&params.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let arguments = Value::Object(params.arguments.into_iter().collect());
        let capability = format!("reverse.{function}");
        let core = {
            let _request_guard = session
                .request_lock
                .lock()
                .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
            self.supervisor.request_worker(
                &session.worker,
                WorkerRequest::ReverseCoreFunction {
                    session_id: params.session_id.clone(),
                    function: function.to_string(),
                    arguments: arguments.clone(),
                },
            )
        };
        let core = match core {
            Ok(WorkerResponse::ReverseCoreFunction { result, .. }) => result,
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let error = worker_failed_message(code, message);
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    &capability,
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: capability.clone(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.record_failed_operation_in_memory(
                    &operation_id,
                    &capability,
                    Some(params.session_id.clone()),
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Ok(other) => {
                let error = format!("unexpected reverse core response: {other:?}");
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    &capability,
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: capability.clone(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.record_failed_operation_in_memory(
                    &operation_id,
                    &capability,
                    Some(params.session_id.clone()),
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Err(error) => {
                let message = error.to_string();
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    &capability,
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: capability.clone(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.record_failed_operation_in_memory(
                    &operation_id,
                    &capability,
                    Some(params.session_id.clone()),
                    message,
                    artifacts,
                    None,
                )?;
                return Err(error);
            }
        };
        record_successful_reverse_core_operation(
            self,
            &workspace,
            &session,
            &params.session_id,
            &operation_id,
            &capability,
            arguments,
            core,
        )
    }

    fn reverse_session_close(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: ReverseSessionCloseParams = parse_params(params)?;
        let session = self.reusable_reverse_session(&params.session_id)?;
        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let close = {
            let _request_guard = session
                .request_lock
                .lock()
                .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
            self.supervisor.request_worker(
                &session.worker,
                WorkerRequest::CloseReverseSession {
                    session_id: params.session_id.clone(),
                },
            )
        };
        match close {
            Ok(WorkerResponse::Ok { .. }) => {
                self.supervisor.close_worker(&session.worker)?;
            }
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let error = worker_failed_message(code, message);
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.session.close",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.close".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Ok(other) => {
                let error = format!("unexpected reverse close response: {other:?}");
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.session.close",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.close".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Err(error) => {
                let message = error.to_string();
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.session.close",
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.close".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message,
                    artifacts,
                    None,
                )?;
                return Err(error);
            }
        }
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "ida".to_string(),
            capability: "reverse.session.close".to_string(),
            status: OperationStatus::Success,
            created_at: Timestamp::now(),
            summary: "reverse session closed".to_string(),
            artifacts: Vec::new(),
            raw_output: None,
        })?;
        let operation = ServiceOperation::success(
            operation_id.clone(),
            "reverse.session.close",
            Some(params.session_id.clone()),
            "reverse session closed",
        );
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        state.sessions.remove(params.session_id.id.as_str());
        Ok(json!({
            "session_id": params.session_id,
            "operation_id": operation_id,
            "operation_status": "success",
            "operation": {
                "status": "success",
                "artifact_refs": [],
                "raw_output_ref": null
            }
        }))
    }

    fn eval_command(
        &self,
        request: EvalDebugCommand,
        capability: &'static str,
    ) -> Result<Value, ServiceError> {
        let session = {
            let state = self.lock_state()?;
            state
                .sessions
                .get(request.session_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(request.session_id.to_string()))?
        };
        if !session.state.is_reusable() {
            return Err(ServiceError::SessionNotReusable(
                request.session_id.to_string(),
            ));
        }
        if session.capability != "debug" {
            return Err(ServiceError::Rpc(format!(
                "session {} is {}, expected debug",
                request.session_id, session.capability
            )));
        }
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = {
            let state = self.lock_state()?;
            state
                .sessions
                .get(request.session_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(request.session_id.to_string()))?
        };
        if !session.state.is_reusable() {
            return Err(ServiceError::SessionNotReusable(
                request.session_id.to_string(),
            ));
        }

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let operation = ServiceOperation::running(
            operation_id.clone(),
            capability,
            Some(session.session_id.clone()),
            format!("{capability} running"),
        );
        {
            let mut state = self.lock_state()?;
            state
                .operations
                .insert(operation_id.id.as_str().to_string(), operation);
        }

        let worker_response = self.request_worker_with_optional_timeout(
            &session,
            WorkerRequest::EvalDebugCommand {
                session_id: session.session_id.clone(),
                operation_id: operation_id.clone(),
                command: request.command.clone(),
                artifact_dir: session.artifact_dir.clone(),
            },
            &operation_id,
            request.timeout_ms,
        );

        self.finish_command_worker_response(
            &session,
            &workspace,
            operation_id,
            capability,
            request.command,
            worker_response,
        )
    }

    fn add_symbols(&self, request: AddSymbolsRequest) -> Result<Value, ServiceError> {
        let session = self.reusable_debug_session(&request.session_id)?;
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = self.reusable_debug_session(&request.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let audit_command = if request.reload {
            format!(".sympath+ {}; .reload", request.symbol_path)
        } else {
            format!(".sympath+ {}", request.symbol_path)
        };
        let operation = ServiceOperation::running(
            operation_id.clone(),
            "debug.add_symbols",
            Some(session.session_id.clone()),
            "debug.add_symbols running",
        );
        {
            let mut state = self.lock_state()?;
            state
                .operations
                .insert(operation_id.id.as_str().to_string(), operation);
        }

        let worker_response = self.supervisor.request_worker(
            &session.worker,
            WorkerRequest::AddSymbols {
                session_id: session.session_id.clone(),
                operation_id: operation_id.clone(),
                symbol_path: request.symbol_path,
                reload: request.reload,
                artifact_dir: session.artifact_dir.clone(),
            },
        );

        self.finish_command_worker_response(
            &session,
            &workspace,
            operation_id,
            "debug.add_symbols",
            audit_command,
            worker_response,
        )
    }

    fn read_memory(&self, request: ReadMemoryRequest) -> Result<Value, ServiceError> {
        let session = self.reusable_debug_session(&request.session_id)?;
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = self.reusable_debug_session(&request.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let audit_command = format!(
            "read_memory address={} length={}",
            request.address, request.length
        );
        let operation = ServiceOperation::running(
            operation_id.clone(),
            "debug.read_memory",
            Some(session.session_id.clone()),
            "debug.read_memory running",
        );
        {
            let mut state = self.lock_state()?;
            state
                .operations
                .insert(operation_id.id.as_str().to_string(), operation);
        }

        let worker_response = self.supervisor.request_worker(
            &session.worker,
            WorkerRequest::ReadMemory {
                session_id: session.session_id.clone(),
                operation_id: operation_id.clone(),
                address: request.address,
                length: request.length,
                artifact_dir: session.artifact_dir.clone(),
            },
        );

        self.finish_memory_worker_response(
            &session,
            &workspace,
            operation_id,
            audit_command,
            worker_response,
        )
    }

    fn finish_command_worker_response(
        &self,
        session: &ManagedSession,
        workspace: &Workspace,
        operation_id: OperationRef,
        capability: &'static str,
        audit_command: String,
        worker_response: Result<WorkerResponse, ServiceError>,
    ) -> Result<Value, ServiceError> {
        match worker_response {
            Ok(WorkerResponse::DebugCommand { mut result, writes }) => {
                let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
                result.operation_id = Some(operation_id.clone());
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                if was_canceled {
                    result.warnings.push("operation was canceled".to_string());
                }
                let workspace_status = if was_canceled {
                    OperationStatus::Canceled
                } else {
                    OperationStatus::Success
                };
                let summary = if was_canceled {
                    format!("{capability} canceled")
                } else {
                    format!("{capability} completed by worker")
                };
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: workspace_status.clone(),
                    created_at: Timestamp::now(),
                    summary: summary.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: capability.to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: workspace_status.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                append_operation_diagnostic_log(
                    capability,
                    &session.session_id,
                    &operation_id,
                    &workspace_status,
                    registered.artifacts.len(),
                    registered.raw_output.as_ref(),
                    None,
                );
                finalize_debug_command_result(&mut result, &registered);
                let response = command_result_response(&result, workspace_status, &registered)?;

                let mut state = self.lock_state()?;
                if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
                    if !was_canceled {
                        operation.status = ServiceOperationStatus::Success;
                    }
                    operation.summary = summary;
                    operation.artifacts = registered.artifacts;
                    operation.raw_output = registered.raw_output;
                    operation.updated_at = Timestamp::now();
                    operation.events.push(ServiceEvent {
                        timestamp: Timestamp::now(),
                        kind: "output".to_string(),
                        message: result.output.clone(),
                    });
                }
                if let Some(session) = state.sessions.get_mut(session.session_id.id.as_str()) {
                    session.last_operation = Some(operation_id.clone());
                    session.updated_at = Timestamp::now();
                }

                Ok(response)
            }
            Ok(WorkerResponse::Failed {
                code,
                message,
                writes,
            }) => {
                let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: capability.to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: OperationStatus::Failed,
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    registered.artifacts.clone(),
                    registered.raw_output.clone(),
                )?;
                append_operation_diagnostic_log(
                    capability,
                    &session.session_id,
                    &operation_id,
                    &OperationStatus::Failed,
                    registered.artifacts.len(),
                    registered.raw_output.as_ref(),
                    Some(&format!("{code}: {message}")),
                );
                Err(ServiceError::Worker(format!("{code}: {message}")))
            }
            Ok(other) => {
                let message = format!("unexpected eval response: {other:?}");
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: capability.to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: OperationStatus::Failed,
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    Vec::new(),
                    None,
                )?;
                append_operation_diagnostic_log(
                    capability,
                    &session.session_id,
                    &operation_id,
                    &OperationStatus::Failed,
                    0,
                    None,
                    Some(&message),
                );
                Err(ServiceError::Worker(message))
            }
            Err(error) => {
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                let status = if was_canceled {
                    OperationStatus::Canceled
                } else {
                    OperationStatus::Failed
                };
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: status.clone(),
                    created_at: Timestamp::now(),
                    summary: error.to_string(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: capability.to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: status.clone(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                if !was_canceled {
                    self.finish_operation_in_memory(
                        &operation_id,
                        ServiceOperationStatus::Failed,
                        error.to_string(),
                        Vec::new(),
                        None,
                    )?;
                }
                append_operation_diagnostic_log(
                    capability,
                    &session.session_id,
                    &operation_id,
                    &status,
                    0,
                    None,
                    Some(&error.to_string()),
                );
                Err(error)
            }
        }
    }

    fn request_worker_with_optional_timeout(
        &self,
        session: &ManagedSession,
        request: WorkerRequest,
        operation_id: &OperationRef,
        timeout_ms: Option<u64>,
    ) -> Result<WorkerResponse, ServiceError> {
        let Some(timeout_ms) = timeout_ms else {
            return self.supervisor.request_worker(&session.worker, request);
        };
        let timeout = Duration::from_millis(timeout_ms);
        let supervisor = Arc::clone(&self.supervisor);
        let worker = session.worker.clone();
        let session_id = session.session_id.clone();
        let operation_id_for_thread = operation_id.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let response = supervisor.request_worker(&worker, request);
            let _ = sender.send(response);
        });

        match receiver.recv_timeout(timeout) {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                append_service_diagnostic_log(&format!(
                    "debug_eval_timeout session_id={} operation_id={} timeout_ms={}",
                    sanitize_log_value(session_id.id.as_str()),
                    sanitize_log_value(operation_id.id.as_str()),
                    timeout_ms
                ));
                if let Err(error) = self.supervisor.kill_worker(&session.worker) {
                    append_service_diagnostic_log(&format!(
                        "debug_eval_timeout_kill_failed session_id={} operation_id={} error={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(&error.to_string())
                    ));
                }
                self.mark_debug_session_error(&session_id)?;
                Err(ServiceError::Worker(format!(
                    "debug operation {} timed out after {} ms; debug worker was killed and the session must be recreated",
                    operation_id_for_thread, timeout_ms
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ServiceError::Worker(
                "worker request ended before returning a response".to_string(),
            )),
        }
    }

    fn finish_memory_worker_response(
        &self,
        session: &ManagedSession,
        workspace: &Workspace,
        operation_id: OperationRef,
        audit_command: String,
        worker_response: Result<WorkerResponse, ServiceError>,
    ) -> Result<Value, ServiceError> {
        match worker_response {
            Ok(WorkerResponse::DebugMemory { mut result, writes }) => {
                let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
                result.operation_id = Some(operation_id.clone());
                result.memory = registered.memory.clone();
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                if was_canceled {
                    result.warnings.push("operation was canceled".to_string());
                }
                let status = if was_canceled {
                    OperationStatus::Canceled
                } else {
                    OperationStatus::Success
                };
                let summary = if was_canceled {
                    "debug.read_memory canceled"
                } else {
                    "debug.read_memory completed by worker"
                };
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: status.clone(),
                    created_at: Timestamp::now(),
                    summary: summary.to_string(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: "debug.read_memory".to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: status.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                append_operation_diagnostic_log(
                    "debug.read_memory",
                    &session.session_id,
                    &operation_id,
                    &status,
                    registered.artifacts.len(),
                    registered.raw_output.as_ref(),
                    None,
                );
                let response = memory_result_response(&result, status, &registered)?;

                let mut state = self.lock_state()?;
                if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
                    if !was_canceled {
                        operation.status = ServiceOperationStatus::Success;
                    }
                    operation.summary = summary.to_string();
                    operation.artifacts = registered.artifacts;
                    operation.raw_output = registered.raw_output;
                    operation.updated_at = Timestamp::now();
                }
                if let Some(session) = state.sessions.get_mut(session.session_id.id.as_str()) {
                    session.last_operation = Some(operation_id.clone());
                    session.updated_at = Timestamp::now();
                }

                Ok(response)
            }
            Ok(WorkerResponse::Failed {
                code,
                message,
                writes,
            }) => {
                let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: "debug.read_memory".to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: OperationStatus::Failed,
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    registered.artifacts.clone(),
                    registered.raw_output.clone(),
                )?;
                append_operation_diagnostic_log(
                    "debug.read_memory",
                    &session.session_id,
                    &operation_id,
                    &OperationStatus::Failed,
                    registered.artifacts.len(),
                    registered.raw_output.as_ref(),
                    Some(&format!("{code}: {message}")),
                );
                Err(ServiceError::Worker(format!("{code}: {message}")))
            }
            Ok(other) => {
                let message = format!("unexpected read memory response: {other:?}");
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: "debug.read_memory".to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: OperationStatus::Failed,
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    Vec::new(),
                    None,
                )?;
                append_operation_diagnostic_log(
                    "debug.read_memory",
                    &session.session_id,
                    &operation_id,
                    &OperationStatus::Failed,
                    0,
                    None,
                    Some(&message),
                );
                Err(ServiceError::Worker(message))
            }
            Err(error) => {
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                let status = if was_canceled {
                    OperationStatus::Canceled
                } else {
                    OperationStatus::Failed
                };
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: status.clone(),
                    created_at: Timestamp::now(),
                    summary: error.to_string(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: "debug.read_memory".to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: status.clone(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                if !was_canceled {
                    self.finish_operation_in_memory(
                        &operation_id,
                        ServiceOperationStatus::Failed,
                        error.to_string(),
                        Vec::new(),
                        None,
                    )?;
                }
                append_operation_diagnostic_log(
                    "debug.read_memory",
                    &session.session_id,
                    &operation_id,
                    &status,
                    0,
                    None,
                    Some(&error.to_string()),
                );
                Err(error)
            }
        }
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, ServiceState>, ServiceError> {
        self.state
            .lock()
            .map_err(|_| ServiceError::Rpc("service state lock poisoned".to_string()))
    }

    fn mark_debug_session_error(&self, session_id: &SessionRef) -> Result<(), ServiceError> {
        let mut state = self.lock_state()?;
        if let Some(session) = state.sessions.get_mut(session_id.id.as_str()) {
            session.state = DebugSessionState::Error;
            session.updated_at = Timestamp::now();
        }
        Ok(())
    }

    fn operation_status(
        &self,
        operation_id: &OperationRef,
    ) -> Result<Option<ServiceOperationStatus>, ServiceError> {
        let state = self.lock_state()?;
        Ok(state
            .operations
            .get(operation_id.id.as_str())
            .map(|operation| operation.status.clone()))
    }

    fn latest_operation_id_from_arguments(&self, arguments: &Value) -> Option<String> {
        let session_id = extract_ref_id_field(arguments, "session_id")?;
        let state = self.state.lock().ok()?;
        state
            .sessions
            .get(&session_id)
            .and_then(|session| session.last_operation.as_ref())
            .map(|operation_id| operation_id.id.as_str().to_string())
    }

    fn start_ttd_recording(
        &self,
        recording_id: RecordingRef,
    ) -> Result<TtdActiveGuard, ServiceError> {
        let mut state = self.lock_state()?;
        if let Some(active) = &state.active_ttd_recording {
            return Err(ServiceError::Rpc(format!(
                "another TTD recording is already active: {active}"
            )));
        }
        state.active_ttd_recording = Some(recording_id.clone());
        Ok(TtdActiveGuard {
            state: self.state.clone(),
            recording_id,
        })
    }

    fn resolve_ttd_exe(&self) -> Result<PathBuf, ServiceError> {
        let ttd_dir = self
            .capabilities
            .ttd_dir
            .clone()
            .or_else(|| {
                RuntimeConfig::default()
                    .resolve_tool_paths()
                    .ttd
                    .map(|location| location.dir)
            })
            .ok_or_else(|| {
                ServiceError::Rpc(
                    "TTD.exe was not found in the automatic tool search path".to_string(),
                )
            })?;
        let ttd_exe = ttd_dir.join("TTD.exe");
        if !ttd_exe.is_file() {
            return Err(ServiceError::Rpc(format!(
                "TTD.exe was not found under {}",
                ttd_dir.display()
            )));
        }
        Ok(ttd_exe)
    }

    fn finish_operation_in_memory(
        &self,
        operation_id: &OperationRef,
        status: ServiceOperationStatus,
        summary: String,
        artifacts: Vec<ArtifactRef>,
        raw_output: Option<ArtifactRef>,
    ) -> Result<(), ServiceError> {
        let mut state = self.lock_state()?;
        if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
            operation.status = status;
            operation.summary = summary;
            operation.artifacts = artifacts;
            operation.raw_output = raw_output;
            operation.updated_at = Timestamp::now();
        }
        Ok(())
    }

    fn record_failed_operation_in_memory(
        &self,
        operation_id: &OperationRef,
        capability: &str,
        session_id: Option<SessionRef>,
        summary: String,
        artifacts: Vec<ArtifactRef>,
        raw_output: Option<ArtifactRef>,
    ) -> Result<(), ServiceError> {
        let now = Timestamp::now();
        let mut operation = ServiceOperation::failed(
            operation_id.clone(),
            capability,
            session_id.clone(),
            summary,
        );
        operation.artifacts = artifacts.clone();
        operation.raw_output = raw_output;
        operation.updated_at = now;

        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        if let Some(session_id) = session_id {
            if let Some(session) = state.sessions.get_mut(session_id.id.as_str()) {
                session.updated_at = now;
                session.last_operation = Some(operation_id.clone());
                session.artifacts.extend(artifacts);
            }
        }
        Ok(())
    }

    fn record_failed_session_create(
        &self,
        workspace: &Workspace,
        operation_id: &OperationRef,
        session_id: &SessionRef,
        summary: impl Into<String>,
    ) -> Result<(), ServiceError> {
        let summary = summary.into();
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "debug.session.create".to_string(),
            status: OperationStatus::Failed,
            created_at: Timestamp::now(),
            summary: summary.clone(),
            artifacts: Vec::new(),
            raw_output: None,
        })?;

        let mut operation = ServiceOperation::failed(
            operation_id.clone(),
            "debug.session.create",
            Some(session_id.clone()),
            summary,
        );
        operation.updated_at = Timestamp::now();
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        Ok(())
    }

    fn reusable_debug_session(
        &self,
        session_id: &SessionRef,
    ) -> Result<ManagedSession, ServiceError> {
        self.reusable_session_with_capability(session_id, "debug")
    }

    fn reusable_reverse_session(
        &self,
        session_id: &SessionRef,
    ) -> Result<ManagedSession, ServiceError> {
        self.reusable_session_with_capability(session_id, "reverse")
    }

    fn reusable_session_with_capability(
        &self,
        session_id: &SessionRef,
        capability: &str,
    ) -> Result<ManagedSession, ServiceError> {
        let state = self.lock_state()?;
        let session = state
            .sessions
            .get(session_id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::SessionNotFound(session_id.to_string()))?;
        if !session.state.is_reusable() {
            return Err(ServiceError::SessionNotReusable(session_id.to_string()));
        }
        if session.capability != capability {
            return Err(ServiceError::Rpc(format!(
                "session {} is {}, expected {}",
                session_id, session.capability, capability
            )));
        }
        Ok(session)
    }
}

impl ServiceCapabilities {
    pub fn from_runtime_config(runtime: &RuntimeConfig) -> Self {
        let tools = runtime.resolve_tool_paths();
        Self::from_runtime_config_and_resolved_tools(runtime, tools)
    }

    pub fn from_runtime_config_with_install_paths(
        runtime: &RuntimeConfig,
        paths: &ServiceInstallPaths,
    ) -> Self {
        let tools =
            runtime.resolve_tool_paths_with_dbgatlas_windbg_runtime_dir(&paths.windbg_runtime_dir);
        Self::from_runtime_config_and_resolved_tools(runtime, tools)
    }

    fn from_runtime_config_and_resolved_tools(
        runtime: &RuntimeConfig,
        tools: dbgatlas_runtime::ResolvedToolPaths,
    ) -> Self {
        Self {
            ida_py_eval: runtime.tools.ida.allow_py_eval,
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
}

struct TtdActiveGuard {
    state: Arc<Mutex<ServiceState>>,
    recording_id: RecordingRef,
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
    root_pid: Option<u32>,
    state: RecordingState,
    created_at: Timestamp,
    updated_at: Timestamp,
    last_operation: Option<OperationRef>,
    artifacts: Vec<ArtifactRef>,
    #[serde(skip)]
    trace_session: Arc<Mutex<Option<dbgatlas_etw::EtwFileSession>>>,
    trace_start_error: Option<String>,
    trace_consumer_error: Option<String>,
    trace_stack_status: Option<dbgatlas_etw::EtwStackTraceStatus>,
    trace_stack_status_error: Option<String>,
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

#[derive(Debug)]
struct ProcessTtdRecorderRunner;

impl TtdRecorderRunner for ProcessTtdRecorderRunner {
    fn run(&self, invocation: TtdRecorderInvocation) -> Result<TtdProcessExit, ServiceError> {
        run_ttd_process(invocation)
    }
}

pub struct MockWorkerSupervisor {
    identity: WorkerIdentity,
    _job: job::ManagedJob,
}

impl MockWorkerSupervisor {
    pub fn new() -> Self {
        Self {
            identity: WorkerIdentity::CurrentUserDevMode,
            _job: job::ManagedJob::create("DbgAtlasMockWorkers"),
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
    job: job::ManagedJob,
}

struct ProcessWorkerState {
    child: Mutex<WorkerProcess>,
    transport: Mutex<WorkerTransport>,
}

enum WorkerProcess {
    Std(Child),
    #[cfg(windows)]
    RawWindows(windows_active_user_process::RawProcess),
}

impl WorkerProcess {
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
}

impl ProcessWorkerSupervisor {
    pub fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            identity: WorkerIdentity::CurrentUserDevMode,
            worker_exe: None,
            workers: Mutex::new(HashMap::new()),
            job: job::ManagedJob::create_result("DbgAtlasDevWorkers")?,
        })
    }

    pub fn new_with_worker_exe(worker_exe: PathBuf) -> Result<Self, ServiceError> {
        Ok(Self {
            identity: WorkerIdentity::CurrentUserDevMode,
            worker_exe: Some(worker_exe),
            workers: Mutex::new(HashMap::new()),
            job: job::ManagedJob::create_result("DbgAtlasDevWorkers")?,
        })
    }

    pub fn new_installed_service() -> Result<Self, ServiceError> {
        Ok(Self {
            identity: WorkerIdentity::LocalSystem,
            worker_exe: None,
            workers: Mutex::new(HashMap::new()),
            job: job::ManagedJob::create_result("DbgAtlasInstalledWorkers")?,
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
            request.session_id.id.as_str(),
            &identity,
            &dbgeng_dirs,
        )
        .map_err(|error| {
            append_worker_create_failed_log(
                &worker_id,
                &request.session_id,
                &identity,
                started,
                "spawn",
                &error.to_string(),
            );
            error
        })?;
        if let Err(error) = self.job.assign_process(&child) {
            let _ = child.kill();
            let _ = child.wait();
            append_worker_create_failed_log(
                &worker_id,
                &request.session_id,
                &identity,
                started,
                "assign_job",
                &error.to_string(),
            );
            return Err(ServiceError::Worker(format!(
                "failed to assign worker process to cleanup job: {error}"
            )));
        }
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
        let state = Arc::new(ProcessWorkerState {
            child: Mutex::new(child),
            transport: Mutex::new(connected),
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
            let mut child = worker_state
                .child
                .lock()
                .map_err(|_| ServiceError::Worker("worker process lock poisoned".to_string()))?;
            let _ = child.wait();
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
            let mut child = worker_state
                .child
                .lock()
                .map_err(|_| ServiceError::Worker("worker process lock poisoned".to_string()))?;
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
                if let Ok(mut child) = worker.child.lock() {
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

fn write_recording_start_artifacts(
    artifact_dir: &Path,
    recording_id: &RecordingRef,
    operation_id: &OperationRef,
    request: &StartRecording,
    root_pid: Option<u32>,
    now: Timestamp,
    trace_start_error: Option<&str>,
    trace_consumer_error: Option<&str>,
    trace_stack_status: Option<dbgatlas_etw::EtwStackTraceStatus>,
    trace_stack_status_error: Option<&str>,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    fs::create_dir_all(artifact_dir.join("events"))?;
    let metadata = recording_metadata_json(
        recording_id,
        operation_id,
        &request.target,
        &request.presets,
        root_pid,
        "running",
        now,
        None,
        etw_adapter_metadata(),
        trace_start_error,
        trace_consumer_error,
        trace_stack_status,
        trace_stack_status_error,
        None,
    );
    let metadata_snapshot = format!("metadata/{}.json", operation_id.id.as_str());
    let metadata_len = write_json_file(&artifact_dir.join(&metadata_snapshot), &metadata)?;
    let _ = write_json_file(&artifact_dir.join("recording.json"), &metadata)?;
    let process_event = normalized_recording_event(
        recording_id,
        operation_id,
        "process",
        "recording_started",
        root_pid,
        now,
    );
    let process_len = write_jsonl_file(
        &artifact_dir.join("events").join("process.jsonl"),
        &process_event,
    )?;

    Ok(vec![
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, &metadata_snapshot),
            kind: "recording.metadata".to_string(),
            byte_len: metadata_len,
            description: Some("recording start metadata snapshot".to_string()),
        },
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "events/process.jsonl"),
            kind: "recording.events.process".to_string(),
            byte_len: process_len,
            description: Some("recording process events".to_string()),
        },
    ])
}

fn write_recording_stop_artifacts(
    artifact_dir: &Path,
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    now: Timestamp,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    fs::create_dir_all(artifact_dir.join("events"))?;
    let mut writes = write_recording_terminal_trace_artifacts(
        artifact_dir,
        recording,
        operation_id,
        now,
        "stopped",
    )?;

    for preset in &recording.presets {
        let suffix = format!("events/{}", preset.artifact_file_name());
        let path = artifact_dir.join(&suffix);
        if path.is_file() && fs::metadata(&path)?.len() > 0 {
            continue;
        }
        let event = normalized_recording_event(
            &recording.recording_id,
            operation_id,
            preset.category(),
            "recording_stopped",
            recording.root_pid,
            now,
        );
        let byte_len = write_jsonl_file(&path, &event)?;
        writes.push(WorkerArtifactWrite {
            relative_path: recording_relative_path(&recording.recording_id, &suffix),
            kind: format!("recording.events.{}", preset.category()),
            byte_len,
            description: Some(format!("recording {} events", preset.category())),
        });
    }

    Ok(writes)
}

struct TraceArtifactOutcome {
    byte_len: u64,
    description: String,
    valid_etl: bool,
    fallback_reason: Option<String>,
    filter: EtwProcessingOutcome,
    extraction: EtwProcessingOutcome,
}

#[derive(Clone, Debug, Default)]
struct EtwProcessingOutcome {
    description: String,
    result: Option<dbgatlas_etw::EtwEventExtractionResult>,
    error: Option<String>,
}

fn write_recording_terminal_trace_artifacts(
    artifact_dir: &Path,
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    now: Timestamp,
    state: &str,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    let trace_path = artifact_dir.join("trace.etl");
    let trace_outcome = write_recording_trace_artifact(recording, &trace_path)?;
    let mut writes = write_recording_terminal_metadata(
        artifact_dir,
        recording,
        operation_id,
        now,
        state,
        Some(&trace_outcome),
    )?;
    writes.push(WorkerArtifactWrite {
        relative_path: recording_relative_path(&recording.recording_id, "trace.etl"),
        kind: "recording.trace".to_string(),
        byte_len: trace_outcome.byte_len,
        description: Some(trace_outcome.description),
    });
    register_recording_event_writes(artifact_dir, recording, &mut writes)?;
    Ok(writes)
}

fn write_recording_trace_artifact(
    recording: &ManagedRecording,
    trace_path: &Path,
) -> Result<TraceArtifactOutcome, ServiceError> {
    let mut session = recording
        .trace_session
        .lock()
        .map_err(|_| ServiceError::Worker("recording trace session lock poisoned".to_string()))?;
    if let Some(session) = session.take() {
        match session.stop() {
            Ok(()) => {
                let filter_note = filter_recording_trace(recording, trace_path);
                let extraction_note = extract_recording_events(recording, trace_path);
                let byte_len = fs::metadata(trace_path)?.len();
                let description = format!(
                    "ETW file trace; {}; {}",
                    filter_note.description, extraction_note.description
                );
                return Ok(TraceArtifactOutcome {
                    byte_len,
                    description,
                    valid_etl: true,
                    fallback_reason: None,
                    filter: filter_note,
                    extraction: extraction_note,
                });
            }
            Err(error) => {
                let trace_text =
                    format!("DbgAtlas fallback trace artifact.\nNative ETW stop failed: {error}\n");
                fs::write(trace_path, trace_text.as_bytes())?;
                return Ok(TraceArtifactOutcome {
                    byte_len: trace_text.len() as u64,
                    description: "fallback trace artifact with native ETW stop error".to_string(),
                    valid_etl: false,
                    fallback_reason: Some(format!("Native ETW stop failed: {error}")),
                    filter: EtwProcessingOutcome::default(),
                    extraction: EtwProcessingOutcome::default(),
                });
            }
        }
    }

    let reason = recording
        .trace_start_error
        .as_deref()
        .unwrap_or("native ETW trace session was not started");
    let trace_text =
        format!("DbgAtlas fallback trace artifact.\nNative ETW start failed: {reason}\n");
    fs::write(trace_path, trace_text.as_bytes())?;
    Ok(TraceArtifactOutcome {
        byte_len: trace_text.len() as u64,
        description: "fallback trace artifact with native ETW start error".to_string(),
        valid_etl: false,
        fallback_reason: Some(format!("Native ETW start failed: {reason}")),
        filter: EtwProcessingOutcome::default(),
        extraction: EtwProcessingOutcome::default(),
    })
}

fn register_recording_event_writes(
    artifact_dir: &Path,
    recording: &ManagedRecording,
    writes: &mut Vec<WorkerArtifactWrite>,
) -> Result<(), ServiceError> {
    for preset in &recording.presets {
        let suffix = format!("events/{}", preset.artifact_file_name());
        let path = artifact_dir.join(&suffix);
        let size_after = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if size_after == 0 {
            continue;
        }
        writes.push(WorkerArtifactWrite {
            relative_path: recording_relative_path(&recording.recording_id, &suffix),
            kind: format!("recording.events.{}", preset.category()),
            byte_len: size_after,
            description: Some(format!("recording {} events", preset.category())),
        });
    }
    Ok(())
}

fn filter_recording_trace(recording: &ManagedRecording, trace_path: &Path) -> EtwProcessingOutcome {
    let filtered_path = recording.artifact_dir.join("trace.filtered.etl");
    match dbgatlas_etw::filter_trace_file(
        trace_path,
        &filtered_path,
        etw_preset_flags(&recording.presets),
        recording.root_pid,
    ) {
        Ok(result) => match fs::copy(&filtered_path, trace_path) {
            Ok(_) => {
                let _ = fs::remove_file(&filtered_path);
                EtwProcessingOutcome {
                    description: format!(
                        "filtered ETL wrote {} events across {} categories and skipped {}",
                        result.events_written, result.files_written, result.skipped_events
                    ),
                    result: Some(result),
                    error: None,
                }
            }
            Err(error) => {
                let _ = fs::remove_file(&filtered_path);
                EtwProcessingOutcome {
                    description: format!("filtered ETL copy failed: {error}"),
                    result: Some(result),
                    error: Some(error.to_string()),
                }
            }
        },
        Err(error) => {
            let _ = fs::remove_file(&filtered_path);
            EtwProcessingOutcome {
                description: format!("filtered ETL fallback to original trace: {error}"),
                result: None,
                error: Some(error.to_string()),
            }
        }
    }
}

fn extract_recording_events(
    recording: &ManagedRecording,
    trace_path: &Path,
) -> EtwProcessingOutcome {
    let events_dir = recording.artifact_dir.join("events");
    match dbgatlas_etw::extract_file_events(
        trace_path,
        &events_dir,
        etw_preset_flags(&recording.presets),
        recording.root_pid,
    ) {
        Ok(result) => EtwProcessingOutcome {
            description: format!(
                "event extraction wrote {} events to {} files and skipped {}",
                result.events_written, result.files_written, result.skipped_events
            ),
            result: Some(result),
            error: None,
        },
        Err(error) => EtwProcessingOutcome {
            description: format!("event extraction skipped: {error}"),
            result: None,
            error: Some(error.to_string()),
        },
    }
}

fn write_recording_terminal_metadata(
    artifact_dir: &Path,
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    now: Timestamp,
    state: &str,
    trace_outcome: Option<&TraceArtifactOutcome>,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    let metadata = recording_metadata_json(
        &recording.recording_id,
        operation_id,
        &recording.target,
        &recording.presets,
        recording.root_pid,
        state,
        recording.created_at,
        Some(now),
        etw_adapter_metadata(),
        recording.trace_start_error.as_deref(),
        recording.trace_consumer_error.as_deref(),
        recording.trace_stack_status,
        recording.trace_stack_status_error.as_deref(),
        trace_outcome,
    );
    let metadata_len = write_json_file(&artifact_dir.join("recording.json"), &metadata)?;
    Ok(vec![WorkerArtifactWrite {
        relative_path: recording_relative_path(&recording.recording_id, "recording.json"),
        kind: "recording.metadata".to_string(),
        byte_len: metadata_len,
        description: Some("recording metadata".to_string()),
    }])
}

fn recording_metadata_json(
    recording_id: &RecordingRef,
    operation_id: &OperationRef,
    target: &RecordingTarget,
    presets: &[RecordingPreset],
    root_pid: Option<u32>,
    state: &str,
    started_at: Timestamp,
    stopped_at: Option<Timestamp>,
    adapter: EtwAdapterMetadata,
    trace_start_error: Option<&str>,
    trace_consumer_error: Option<&str>,
    trace_stack_status: Option<dbgatlas_etw::EtwStackTraceStatus>,
    trace_stack_status_error: Option<&str>,
    trace_outcome: Option<&TraceArtifactOutcome>,
) -> Value {
    json!({
        "recording_id": recording_id,
        "target": target,
        "mode": target.mode(),
        "root_pid": root_pid,
        "process_tree_filter": {
            "root_pid": root_pid,
            "mode": "process_tree"
        },
        "presets": presets,
        "etw_preset_flags": etw_preset_flags(presets).bits(),
        "state": state,
        "started_at": started_at,
        "stopped_at": stopped_at,
        "adapter": adapter,
        "stack_trace": stack_trace_metadata(
            trace_stack_status,
            trace_stack_status_error,
            trace_start_error,
        ),
        "trace_start_error": trace_start_error,
        "trace_consumer_error": trace_consumer_error,
        "trace": trace_metadata(trace_outcome),
        "event_extraction": event_extraction_metadata(trace_outcome),
        "operation_id": operation_id,
    })
}

fn trace_metadata(outcome: Option<&TraceArtifactOutcome>) -> Value {
    match outcome {
        Some(outcome) => json!({
            "valid_etl": outcome.valid_etl,
            "fallback_reason": outcome.fallback_reason,
            "filter": processing_metadata(&outcome.filter),
        }),
        None => json!({
            "valid_etl": null,
            "fallback_reason": null,
            "filter": null,
        }),
    }
}

fn event_extraction_metadata(outcome: Option<&TraceArtifactOutcome>) -> Value {
    match outcome {
        Some(outcome) => processing_metadata(&outcome.extraction),
        None => json!(null),
    }
}

fn processing_metadata(outcome: &EtwProcessingOutcome) -> Value {
    let warnings = outcome.result.map(extraction_warnings).unwrap_or_default();
    json!({
        "description": outcome.description,
        "error": outcome.error,
        "result": outcome.result,
        "warnings": warnings,
    })
}

fn extraction_warnings(result: dbgatlas_etw::EtwEventExtractionResult) -> Vec<String> {
    let mut warnings = Vec::new();
    if result.unmatched_op_end > 0 {
        warnings.push(format!(
            "native ETW file_io saw {} OpEnd events without a matching begin event",
            result.unmatched_op_end
        ));
    }
    if result.reused_irp > 0 {
        warnings.push(format!(
            "native ETW file_io saw {} reused IrpPtr values before a matching OpEnd",
            result.reused_irp
        ));
    }
    if result.incomplete_io > 0 {
        warnings.push(format!(
            "native ETW file_io left {} begin events without a matching OpEnd",
            result.incomplete_io
        ));
    }
    if result.file_path_unresolved > 0 {
        warnings.push(format!(
            "native ETW file_io left {} target file events without a resolved path",
            result.file_path_unresolved
        ));
    }
    if result.dropped_stack_walk > 0 {
        warnings.push(format!(
            "native ETW dropped {} unmatched StackWalk events while bounding the pending stack cache",
            result.dropped_stack_walk
        ));
    }
    warnings
}

fn stack_trace_metadata(
    status: Option<dbgatlas_etw::EtwStackTraceStatus>,
    status_error: Option<&str>,
    trace_start_error: Option<&str>,
) -> Value {
    let mut warnings = Vec::new();
    if let Some(error) = trace_start_error {
        warnings.push(format!("native ETW trace session was not started: {error}"));
    }
    if let Some(error) = status_error {
        warnings.push(format!("stack trace status unavailable: {error}"));
    }
    if let Some(status) = status {
        if status.provider_stack_warning_count > 0 {
            warnings.push(format!(
                "stack trace provider enable fell back without stack for {} provider(s)",
                status.provider_stack_warning_count
            ));
        }
        if status.kernel_stack_warning_count > 0 {
            warnings.push(format!(
                "kernel stack tracing enable failed for {} configuration attempt(s)",
                status.kernel_stack_warning_count
            ));
        }
        json!({
            "requested": status.requested,
            "enabled": status.enabled,
            "provider_stack_enabled": status.provider_stack_enabled,
            "kernel_stack_enabled": status.kernel_stack_enabled,
            "warnings": warnings,
        })
    } else {
        json!({
            "requested": true,
            "enabled": false,
            "provider_stack_enabled": false,
            "kernel_stack_enabled": false,
            "warnings": warnings,
        })
    }
}

fn etw_preset_flags(presets: &[RecordingPreset]) -> dbgatlas_etw::EtwPresetFlags {
    let mut flags = dbgatlas_etw::EtwPresetFlags::empty();
    for preset in presets {
        flags.insert(match preset {
            RecordingPreset::Process => dbgatlas_etw::EtwPresetFlags::PROCESS,
            RecordingPreset::Thread => dbgatlas_etw::EtwPresetFlags::THREAD,
            RecordingPreset::Image => dbgatlas_etw::EtwPresetFlags::IMAGE,
            RecordingPreset::File => dbgatlas_etw::EtwPresetFlags::FILE,
            RecordingPreset::Registry => dbgatlas_etw::EtwPresetFlags::REGISTRY,
            RecordingPreset::Network => dbgatlas_etw::EtwPresetFlags::NETWORK,
        });
    }
    flags
}

#[derive(Clone, Debug, Serialize)]
struct EtwAdapterMetadata {
    id: &'static str,
    native: &'static str,
    version: Option<dbgatlas_etw::NativeVersion>,
    capabilities: Option<dbgatlas_etw::EtwCapabilities>,
    note: String,
}

fn etw_adapter_metadata() -> EtwAdapterMetadata {
    match dbgatlas_etw::adapter_info() {
        Ok(info) => EtwAdapterMetadata {
            id: "etw",
            native: "available",
            version: Some(info.version),
            capabilities: Some(info.capabilities),
            note:
                "native ETW adapter ABI is available; file trace and ETL event extraction are wired"
                    .to_string(),
        },
        Err(error) => EtwAdapterMetadata {
            id: "etw",
            native: "unavailable",
            version: None,
            capabilities: None,
            note: error.to_string(),
        },
    }
}

fn normalized_recording_event(
    recording_id: &RecordingRef,
    operation_id: &OperationRef,
    category: &str,
    event_type: &str,
    pid: Option<u32>,
    timestamp: Timestamp,
) -> Value {
    json!({
        "schema_version": 1,
        "recording_id": recording_id,
        "timestamp": timestamp,
        "category": category,
        "event_type": event_type,
        "pid": pid,
        "tid": null,
        "process": {
            "pid": pid,
            "parent_pid": null,
            "image_path": null,
            "command_line": null
        },
        "operation_id": operation_id,
        "artifact_id": null,
        "etw": {
            "provider": "dbg_atlas_placeholder",
            "event_id": 0,
            "version": 1,
            "opcode": event_type,
            "keywords": [category],
            "raw": {}
        }
    })
}

enum PreparedRecordingTarget {
    Attach { pid: u32 },
    Launch(PreparedLaunch),
}

impl PreparedRecordingTarget {
    fn root_pid(&self) -> u32 {
        match self {
            Self::Attach { pid } => *pid,
            Self::Launch(launch) => launch.pid(),
        }
    }

    fn resume(self) -> Result<(), ServiceError> {
        match self {
            Self::Attach { .. } => Ok(()),
            Self::Launch(launch) => launch.resume(),
        }
    }
}

fn prepare_recording_target(
    target: &RecordingTarget,
) -> Result<PreparedRecordingTarget, ServiceError> {
    match target {
        RecordingTarget::Launch { executable, args } => Ok(PreparedRecordingTarget::Launch(
            PreparedLaunch::create(executable, args)?,
        )),
        RecordingTarget::Attach { pid } => Ok(PreparedRecordingTarget::Attach { pid: *pid }),
    }
}

enum TtdRecorderProcess {
    Std(Child),
    #[cfg(windows)]
    RawWindows(windows_active_user_process::RawProcess),
}

impl TtdRecorderProcess {
    fn try_wait(&mut self) -> Result<Option<Option<i32>>, std::io::Error> {
        match self {
            Self::Std(child) => child
                .try_wait()
                .map(|status| status.map(|status| status.code())),
            #[cfg(windows)]
            Self::RawWindows(process) => process.try_wait(),
        }
    }

    fn wait(&mut self) -> Result<Option<i32>, std::io::Error> {
        match self {
            Self::Std(child) => child.wait().map(|status| status.code()),
            #[cfg(windows)]
            Self::RawWindows(process) => process.wait_code(),
        }
    }

    fn kill(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::Std(child) => child.kill(),
            #[cfg(windows)]
            Self::RawWindows(process) => process.kill(),
        }
    }
}

fn spawn_ttd_process(
    ttd_exe: &Path,
    args: &[OsString],
    worker_identity: TtdRecorderIdentity,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<TtdRecorderProcess, ServiceError> {
    if let Some(parent) = stdout_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = stderr_path.parent() {
        fs::create_dir_all(parent)?;
    }
    match worker_identity {
        TtdRecorderIdentity::Default => {
            let stdout = fs::File::create(stdout_path)?;
            let stderr = fs::File::create(stderr_path)?;
            Ok(TtdRecorderProcess::Std(
                Command::new(ttd_exe)
                    .args(args)
                    .stdout(Stdio::from(stdout))
                    .stderr(Stdio::from(stderr))
                    .spawn()?,
            ))
        }
        TtdRecorderIdentity::ActiveInteractiveUser => {
            spawn_active_interactive_ttd_process(ttd_exe, args, stdout_path, stderr_path)
        }
    }
}

#[cfg(windows)]
fn spawn_active_interactive_ttd_process(
    ttd_exe: &Path,
    args: &[OsString],
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<TtdRecorderProcess, ServiceError> {
    let helper_exe = std::env::current_exe()?;
    let helper_args = ttd_command_helper_args(ttd_exe, args, stdout_path, stderr_path);
    windows_active_user_process::spawn_os(&helper_exe, &helper_args)
        .map(TtdRecorderProcess::RawWindows)
}

fn ttd_command_helper_args(
    ttd_exe: &Path,
    args: &[OsString],
    stdout_path: &Path,
    stderr_path: &Path,
) -> Vec<OsString> {
    let mut helper_args = vec![
        OsString::from("service"),
        OsString::from("run-ttd-command"),
        OsString::from("--executable"),
        ttd_exe.as_os_str().to_os_string(),
        OsString::from("--stdout-path"),
        stdout_path.as_os_str().to_os_string(),
        OsString::from("--stderr-path"),
        stderr_path.as_os_str().to_os_string(),
        OsString::from("--"),
    ];
    helper_args.extend(args.iter().cloned());
    helper_args
}

#[cfg(not(windows))]
fn spawn_active_interactive_ttd_process(
    _ttd_exe: &Path,
    _args: &[OsString],
    _stdout_path: &Path,
    _stderr_path: &Path,
) -> Result<TtdRecorderProcess, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}

fn run_ttd_process(invocation: TtdRecorderInvocation) -> Result<TtdProcessExit, ServiceError> {
    let mut child = spawn_ttd_process(
        &invocation.ttd_exe,
        &invocation.args,
        invocation.worker_identity,
        &invocation.stdout_path,
        &invocation.stderr_path,
    )?;
    let deadline = Instant::now() + invocation.timeout;
    loop {
        if let Some(exit_code) = child.try_wait()? {
            return Ok(TtdProcessExit {
                exit_code,
                timed_out: false,
                stop: None,
                killed_after_timeout: false,
            });
        }
        if Instant::now() >= deadline {
            let mut stop = None;
            if let Some(timeout_stop) = invocation.timeout_stop.as_ref() {
                let recorder_exit_deadline = Instant::now() + timeout_stop.recorder_exit_timeout;
                let mut timeout_stop = timeout_stop.clone();
                if timeout_stop.prefer_recorded_pid {
                    let recorder_output = read_text_lossy(&invocation.stdout_path)
                        + "\n"
                        + read_text_lossy(&invocation.stderr_path).as_str();
                    if let Some(pid) = parse_first_recorded_pid(&recorder_output) {
                        timeout_stop.stop_target = OsString::from(pid.to_string());
                    }
                }
                stop = Some(run_ttd_stop_process(
                    &invocation.ttd_exe,
                    &timeout_stop,
                    invocation.worker_identity,
                ));
                while Instant::now() < recorder_exit_deadline {
                    if let Some(exit_code) = child.try_wait()? {
                        return Ok(TtdProcessExit {
                            exit_code,
                            timed_out: true,
                            stop,
                            killed_after_timeout: false,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            let (exit_code, killed_after_timeout) = if let Some(exit_code) = child.try_wait()? {
                (exit_code, false)
            } else {
                let _ = child.kill();
                (child.wait().ok().flatten(), true)
            };
            return Ok(TtdProcessExit {
                exit_code,
                timed_out: true,
                stop,
                killed_after_timeout,
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn run_ttd_stop_process(
    ttd_exe: &Path,
    timeout_stop: &TtdTimeoutStop,
    worker_identity: TtdRecorderIdentity,
) -> TtdStopExit {
    let args = ttd_stop_args(&timeout_stop.stop_target);
    match run_ttd_command(
        ttd_exe,
        &args,
        worker_identity,
        timeout_stop.timeout,
        &timeout_stop.stdout_path,
        &timeout_stop.stderr_path,
    ) {
        Ok(exit) => TtdStopExit {
            stop_target: timeout_stop.stop_target.clone(),
            exit_code: exit.exit_code,
            timed_out: exit.timed_out,
            error: None,
        },
        Err(error) => {
            let _ = ensure_file_exists(&timeout_stop.stdout_path);
            let _ = ensure_file_exists(&timeout_stop.stderr_path);
            TtdStopExit {
                stop_target: timeout_stop.stop_target.clone(),
                exit_code: None,
                timed_out: false,
                error: Some(error.to_string()),
            }
        }
    }
}

fn ttd_stop_args(stop_target: &OsStr) -> Vec<OsString> {
    vec![OsString::from("-stop"), stop_target.to_os_string()]
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TtdCommandExit {
    exit_code: Option<i32>,
    timed_out: bool,
}

fn run_ttd_command(
    ttd_exe: &Path,
    args: &[OsString],
    worker_identity: TtdRecorderIdentity,
    timeout: Duration,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<TtdCommandExit, ServiceError> {
    let mut child = spawn_ttd_process(ttd_exe, args, worker_identity, stdout_path, stderr_path)?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(exit_code) = child.try_wait()? {
            return Ok(TtdCommandExit {
                exit_code,
                timed_out: false,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let exit_code = child.wait().ok().flatten();
            return Ok(TtdCommandExit {
                exit_code,
                timed_out: true,
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[derive(Clone, Debug, Default)]
struct DiscoveredTtdArtifacts {
    traces: Vec<PathBuf>,
    trace_indexes: Vec<PathBuf>,
    recorder_logs: Vec<PathBuf>,
}

struct TtdRecordingMetadata<'a> {
    recording_id: &'a RecordingRef,
    operation_id: &'a OperationRef,
    request: &'a RecordTtd,
    status: &'a str,
    operation_status: &'a OperationStatus,
    target_pid: Option<u32>,
    recorder_exit_code: Option<i32>,
    timed_out: bool,
    started_at: Timestamp,
    stopped_at: Timestamp,
    duration_ms: u64,
    ttd_exe: &'a Path,
    worker_identity: TtdRecorderIdentity,
    discovered: &'a DiscoveredTtdArtifacts,
    warnings: &'a [String],
    error: Option<&'a str>,
}

fn append_ttd_recording_event(
    events_path: &Path,
    recording_id: &RecordingRef,
    operation_id: &OperationRef,
    event: &str,
    timestamp: Timestamp,
    fields: Value,
    error: Option<String>,
) -> Result<u64, ServiceError> {
    write_jsonl_file(
        events_path,
        &json!({
            "event": event,
            "recording_id": recording_id,
            "operation_id": operation_id,
            "timestamp": timestamp,
            "fields": fields,
            "error": error,
        }),
    )
}

fn discover_ttd_artifacts(traces_dir: &Path) -> Result<DiscoveredTtdArtifacts, ServiceError> {
    let mut discovered = DiscoveredTtdArtifacts::default();
    if !traces_dir.exists() {
        return Ok(discovered);
    }
    for entry in fs::read_dir(traces_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "run" => discovered.traces.push(path),
            "idx" => discovered.trace_indexes.push(path),
            "out" | "err" | "log" => discovered.recorder_logs.push(path),
            _ => {}
        }
    }
    discovered.traces.sort();
    discovered.trace_indexes.sort();
    discovered.recorder_logs.sort();
    Ok(discovered)
}

fn ttd_recording_metadata_json(input: TtdRecordingMetadata<'_>) -> Value {
    json!({
        "recording_id": input.recording_id,
        "operation_id": input.operation_id,
        "target": input.request.target,
        "mode": input.request.target.mode(),
        "worker_identity": input.worker_identity,
        "timeout_ms": input.request.timeout_ms,
        "options": input.request.options,
        "target_pid": input.target_pid,
        "started_at": input.started_at,
        "stopped_at": input.stopped_at,
        "duration_ms": input.duration_ms,
        "status": input.status,
        "operation_status": input.operation_status,
        "recorder_exit_code": input.recorder_exit_code,
        "timed_out": input.timed_out,
        "adapter": {
            "kind": "ttd",
            "tool": "TTD.exe",
            "ttd_exe": input.ttd_exe,
            "worker_identity": input.worker_identity,
        },
        "traces": input.discovered.traces,
        "trace_indexes": input.discovered.trace_indexes,
        "recorder_logs": input.discovered.recorder_logs,
        "warnings": input.warnings,
        "error": input.error,
    })
}

fn ttd_recording_writes(
    recording_id: &RecordingRef,
    artifact_dir: &Path,
    metadata_len: u64,
    include_stop_stdout: bool,
    include_stop_stderr: bool,
    discovered: &DiscoveredTtdArtifacts,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    let mut writes = vec![
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recording.json"),
            kind: "recording.metadata".to_string(),
            byte_len: metadata_len,
            description: Some("TTD recording metadata".to_string()),
        },
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "events.jsonl"),
            kind: "recording.events.ttd".to_string(),
            byte_len: file_len_or_zero(&artifact_dir.join("events.jsonl"))?,
            description: Some("TTD recording events".to_string()),
        },
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recorder.stdout.txt"),
            kind: "recording.recorder_output".to_string(),
            byte_len: file_len_or_zero(&artifact_dir.join("recorder.stdout.txt"))?,
            description: Some("TTD recorder stdout".to_string()),
        },
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recorder.stderr.txt"),
            kind: "recording.recorder_output".to_string(),
            byte_len: file_len_or_zero(&artifact_dir.join("recorder.stderr.txt"))?,
            description: Some("TTD recorder stderr".to_string()),
        },
    ];
    if include_stop_stdout {
        writes.push(WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recorder-stop.stdout.txt"),
            kind: "recording.recorder_output".to_string(),
            byte_len: file_len_or_zero(&artifact_dir.join("recorder-stop.stdout.txt"))?,
            description: Some("TTD recorder stop stdout".to_string()),
        });
    }
    if include_stop_stderr {
        writes.push(WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recorder-stop.stderr.txt"),
            kind: "recording.recorder_output".to_string(),
            byte_len: file_len_or_zero(&artifact_dir.join("recorder-stop.stderr.txt"))?,
            description: Some("TTD recorder stop stderr".to_string()),
        });
    }
    for trace in &discovered.traces {
        writes.push(discovered_ttd_write(
            recording_id,
            artifact_dir,
            trace,
            "recording.ttd.trace",
            "TTD .run trace",
        )?);
    }
    for index in &discovered.trace_indexes {
        writes.push(discovered_ttd_write(
            recording_id,
            artifact_dir,
            index,
            "recording.ttd.index",
            "TTD trace index",
        )?);
    }
    for log in &discovered.recorder_logs {
        writes.push(discovered_ttd_write(
            recording_id,
            artifact_dir,
            log,
            "recording.recorder_output",
            "TTD recorder log",
        )?);
    }
    Ok(writes)
}

fn ttd_trace_artifact_results(
    workspace: &Workspace,
    writes: &[WorkerArtifactWrite],
    registered: &RegisteredWorkerWrites,
) -> Vec<Value> {
    writes
        .iter()
        .zip(registered.artifacts.iter())
        .filter(|(write, _)| write.kind == "recording.ttd.trace")
        .map(|(write, artifact_ref)| {
            json!({
                "path": workspace.root().join(&write.relative_path),
                "relative_path": &write.relative_path,
                "artifact_ref": artifact_ref,
                "byte_len": write.byte_len,
            })
        })
        .collect()
}

fn discovered_ttd_write(
    recording_id: &RecordingRef,
    artifact_dir: &Path,
    path: &Path,
    kind: &str,
    description: &str,
) -> Result<WorkerArtifactWrite, ServiceError> {
    let relative_to_recording = path.strip_prefix(artifact_dir).map_err(|_| {
        ServiceError::Rpc(format!(
            "TTD artifact escaped recording dir: {}",
            path.display()
        ))
    })?;
    Ok(WorkerArtifactWrite {
        relative_path: recording_relative_path(
            recording_id,
            &relative_to_recording.to_string_lossy(),
        ),
        kind: kind.to_string(),
        byte_len: fs::metadata(path)?.len(),
        description: Some(description.to_string()),
    })
}

fn file_len_or_zero(path: &Path) -> Result<u64, ServiceError> {
    if !path.exists() {
        return Ok(0);
    }
    Ok(fs::metadata(path)?.len())
}

fn ensure_file_exists(path: &Path) -> Result<(), ServiceError> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, [])?;
    Ok(())
}

fn parse_first_recorded_pid(text: &str) -> Option<u32> {
    for line in text.lines() {
        if let Some(index) = line.find("PID:") {
            let digits = line[index + 4..]
                .chars()
                .skip_while(|ch| ch.is_whitespace())
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>();
            if let Ok(pid) = digits.parse::<u32>() {
                return Some(pid);
            }
        }
        if line.contains("Recording process ") {
            if let Some(open) = line.rfind('(') {
                if let Some(close) = line[open + 1..].find(')') {
                    let value = &line[open + 1..open + 1 + close];
                    if let Ok(pid) = value.parse::<u32>() {
                        return Some(pid);
                    }
                }
            }
        }
    }
    None
}

fn recorder_error_summary(
    stdout_path: &Path,
    stderr_path: &Path,
    exit_code: Option<i32>,
) -> String {
    let stderr = read_text_lossy(stderr_path);
    if !stderr.trim().is_empty() {
        return format!(
            "TTD recorder exited with code {:?}: {}",
            exit_code,
            last_non_empty_line(&stderr)
        );
    }
    let stdout = read_text_lossy(stdout_path);
    if !stdout.trim().is_empty() {
        return format!(
            "TTD recorder exited with code {:?}: {}",
            exit_code,
            last_non_empty_line(&stdout)
        );
    }
    format!("TTD recorder exited with code {:?}", exit_code)
}

fn read_text_lossy(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn last_non_empty_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

enum PreparedLaunch {
    #[cfg(windows)]
    Suspended(suspended_process::SuspendedProcess),
    #[cfg(not(windows))]
    Direct { pid: u32 },
}

impl PreparedLaunch {
    fn create(executable: &Path, args: &[String]) -> Result<Self, ServiceError> {
        #[cfg(windows)]
        {
            return suspended_process::SuspendedProcess::create(executable, args)
                .map(Self::Suspended)
                .map_err(ServiceError::Io);
        }

        #[cfg(not(windows))]
        {
            let child = Command::new(executable).args(args).spawn()?;
            Ok(Self::Direct { pid: child.id() })
        }
    }

    fn pid(&self) -> u32 {
        match self {
            #[cfg(windows)]
            Self::Suspended(process) => process.pid(),
            #[cfg(not(windows))]
            Self::Direct { pid } => *pid,
        }
    }

    fn resume(self) -> Result<(), ServiceError> {
        match self {
            #[cfg(windows)]
            Self::Suspended(process) => process.resume().map_err(ServiceError::Io),
            #[cfg(not(windows))]
            Self::Direct { .. } => Ok(()),
        }
    }
}

fn write_json_file(path: &Path, value: &Value) -> Result<u64, ServiceError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, &bytes)?;
    Ok(bytes.len() as u64)
}

fn write_jsonl_file(path: &Path, value: &Value) -> Result<u64, ServiceError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&bytes)?;
    Ok(bytes.len() as u64)
}

fn record_failed_reverse_operation(
    workspace: &Workspace,
    session_id: &SessionRef,
    artifact_dir: &Path,
    operation_id: &OperationRef,
    capability: &str,
    message: &str,
) -> Result<Vec<ArtifactRef>, ServiceError> {
    let value = json!({
        "operation_id": operation_id,
        "session_id": session_id,
        "capability": capability,
        "error": message,
        "created_at": Timestamp::now()
    });
    let error_file = format!("errors/{}.json", operation_id.id.as_str());
    let byte_len = write_json_file(&artifact_dir.join(&error_file), &value)?;
    let artifact_id = next_artifact_ref();
    workspace.register_artifact(&ArtifactMetadata {
        artifact_id: artifact_id.clone(),
        kind: "reverse.adapter_error".to_string(),
        relative_path: reverse_relative_path(session_id, &error_file),
        created_at: Timestamp::now(),
        operation_id: Some(operation_id.clone()),
        byte_len: Some(byte_len),
        description: Some("IDA adapter error".to_string()),
    })?;
    Ok(vec![artifact_id])
}

fn record_successful_reverse_core_operation(
    host: &ServiceHost,
    workspace: &Workspace,
    session: &ManagedSession,
    session_id: &SessionRef,
    operation_id: &OperationRef,
    capability: &str,
    arguments: Value,
    core: ReverseCoreFunctionResult,
) -> Result<Value, ServiceError> {
    let now = Timestamp::now();
    let event = json!({
        "operation_id": operation_id,
        "session_id": session_id,
        "created_at": now,
        "function": core.function,
        "arguments": arguments,
        "result": core.result,
        "warnings": core.warnings,
    });
    let core_file = format!("core/{}.jsonl", operation_id.id.as_str());
    let byte_len = write_jsonl_file(&session.artifact_dir.join(&core_file), &event)?;
    let artifact_id = next_artifact_ref();
    let response_result = reverse_core_response_result(&core, &artifact_id, byte_len)?;
    workspace.register_artifact(&ArtifactMetadata {
        artifact_id: artifact_id.clone(),
        kind: "reverse.core".to_string(),
        relative_path: reverse_relative_path(session_id, &core_file),
        created_at: now,
        operation_id: Some(operation_id.clone()),
        byte_len: Some(byte_len),
        description: Some(format!("IDA Core Function {} result", core.function)),
    })?;
    workspace.append_operation(&OperationRecord {
        operation_id: operation_id.clone(),
        adapter_id: "ida".to_string(),
        capability: capability.to_string(),
        status: OperationStatus::Success,
        created_at: now,
        summary: format!("IDA Core Function {} completed", core.function),
        artifacts: vec![artifact_id.clone()],
        raw_output: None,
    })?;

    let mut operation = ServiceOperation::success(
        operation_id.clone(),
        capability,
        Some(session_id.clone()),
        "reverse core function completed",
    );
    operation.artifacts = vec![artifact_id.clone()];
    let mut state = host.lock_state()?;
    state
        .operations
        .insert(operation_id.id.as_str().to_string(), operation);
    if let Some(session) = state.sessions.get_mut(session_id.id.as_str()) {
        session.updated_at = now;
        session.last_operation = Some(operation_id.clone());
        session.artifacts.push(artifact_id.clone());
    }

    Ok(json!({
        "session_id": session_id,
        "operation_id": operation_id,
        "operation_status": "success",
        "artifact_refs": [artifact_id],
        "function": core.function,
        "result": response_result,
        "warnings": core.warnings,
        "operation": {
            "status": "success",
            "artifact_refs": [artifact_id],
            "raw_output_ref": null
        }
    }))
}

fn reverse_core_response_result(
    core: &ReverseCoreFunctionResult,
    artifact_id: &ArtifactRef,
    artifact_byte_len: u64,
) -> Result<Value, ServiceError> {
    if core.function != "decompile"
        || serde_json::to_vec(&core.result)?.len() <= MAX_INLINE_REVERSE_RESULT_BYTES
    {
        return Ok(core.result.clone());
    }

    let mut result = core.result.clone();
    if let Value::Object(object) = &mut result {
        if let Some(Value::String(pseudocode)) = object.get_mut("pseudocode") {
            if pseudocode.len() > MAX_INLINE_DECOMPILE_PSEUDOCODE_BYTES {
                *pseudocode =
                    truncate_utf8_bytes(pseudocode, MAX_INLINE_DECOMPILE_PSEUDOCODE_BYTES)
                        .to_string();
            }
        }
        object.insert("pseudocode_truncated".to_string(), json!(true));
        object.insert(
            "full_result_artifact_ref".to_string(),
            serde_json::to_value(artifact_id)?,
        );
        object.insert("full_result_byte_len".to_string(), json!(artifact_byte_len));
        return Ok(result);
    }

    Ok(json!({
        "pseudocode_truncated": true,
        "preview": result,
        "full_result_artifact_ref": artifact_id,
        "full_result_byte_len": artifact_byte_len,
    }))
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn recording_response(
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    Ok(json!({
        "recording_id": recording.recording_id,
        "state": recording.state,
        "target": recording.target,
        "root_pid": recording.root_pid,
        "presets": recording.presets,
        "operation_id": operation_id,
        "operation_status": status,
        "artifact_refs": writes.artifacts,
        "raw_output_ref": writes.raw_output,
        "operation": {
            "status": status,
            "artifact_refs": writes.artifacts,
            "raw_output_ref": writes.raw_output,
        }
    }))
}

fn recording_status_response(recording: &ManagedRecording) -> Value {
    json!({
        "recording_id": recording.recording_id,
        "state": recording.state,
        "target": recording.target,
        "root_pid": recording.root_pid,
        "presets": recording.presets,
        "created_at": recording.created_at,
        "updated_at": recording.updated_at,
        "last_operation": recording.last_operation,
        "artifact_refs": recording.artifacts,
    })
}

fn mock_worker_response(request: WorkerRequest) -> Result<WorkerResponse, ServiceError> {
    match request {
        WorkerRequest::StartDebugSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session started by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::EvalDebugCommand {
            session_id,
            operation_id,
            command,
            artifact_dir,
        } => write_mock_eval_response(session_id, operation_id, command, artifact_dir),
        WorkerRequest::AddSymbols {
            session_id,
            operation_id,
            symbol_path,
            reload,
            artifact_dir,
        } => {
            let command = if reload {
                format!(".sympath+ {symbol_path}; .reload")
            } else {
                format!(".sympath+ {symbol_path}")
            };
            write_mock_eval_response(session_id, operation_id, command, artifact_dir)
        }
        WorkerRequest::ReadMemory {
            session_id,
            operation_id,
            address,
            length,
            artifact_dir,
        } => write_mock_memory_response(session_id, operation_id, address, length, artifact_dir),
        WorkerRequest::OpenReverseSession { .. } => {
            Ok(WorkerResponse::ReverseSessionOpened { writes: Vec::new() })
        }
        WorkerRequest::LookupReverseFunction {
            runtime_address,
            runtime_module_base,
            ida_image_base,
            ..
        } => {
            if runtime_address < runtime_module_base {
                return Ok(WorkerResponse::Failed {
                    code: "reverse_lookup_failed".to_string(),
                    message: "runtime_address is below runtime_module_base".to_string(),
                    writes: Vec::new(),
                });
            }
            let rva = runtime_address - runtime_module_base;
            let ida_ea = match ida_image_base.checked_add(rva) {
                Some(value) => value,
                None => {
                    return Ok(WorkerResponse::Failed {
                        code: "reverse_lookup_failed".to_string(),
                        message: "ida_ea overflow".to_string(),
                        writes: Vec::new(),
                    });
                }
            };
            Ok(WorkerResponse::ReverseFunctionLookup {
                result: ReverseFunctionLookupResult {
                    runtime_address,
                    runtime_module_base,
                    rva,
                    ida_image_base,
                    ida_ea,
                    function_start: ida_ea & !0xff,
                    function_end: (ida_ea & !0xff) + 0x100,
                    function_name: format!("mock_function_{:x}", ida_ea & !0xff),
                    found: true,
                },
                writes: Vec::new(),
            })
        }
        WorkerRequest::ReverseCoreFunction {
            function,
            arguments,
            ..
        } => mock_reverse_core_response(function, arguments),
        WorkerRequest::CloseReverseSession { .. } => Ok(WorkerResponse::Ok {
            summary: "reverse session closed by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::CloseSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session closed by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::KillSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session killed by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::CancelOperation { .. } => Ok(WorkerResponse::Ok {
            summary: "operation canceled by mock worker".to_string(),
            writes: Vec::new(),
        }),
    }
}

fn mock_reverse_core_response(
    function: String,
    arguments: Value,
) -> Result<WorkerResponse, ServiceError> {
    let result = match function.as_str() {
        "lookup_funcs" => mock_lookup_funcs(&arguments)?,
        "int_convert" => mock_int_convert(&arguments),
        "list_funcs" => mock_list_funcs(&arguments)?,
        "list_globals" => mock_list_globals(&arguments)?,
        "list_imports" => mock_list_imports(&arguments)?,
        "list_strings" => mock_list_strings(&arguments)?,
        "get_string" => mock_get_string(&arguments)?,
        "get_bytes" => mock_get_bytes(&arguments)?,
        "get_int" => mock_get_int(&arguments)?,
        "decompile" => mock_decompile(&arguments)?,
        "disasm" => mock_disasm(&arguments)?,
        "xrefs_to" => mock_xrefs_to(&arguments)?,
        "xrefs_to_field" => mock_xrefs_to_field(&arguments)?,
        "callees" => mock_callees(&arguments)?,
        "rename" => mock_batch_write_result_with_diagnostics(&arguments, "items"),
        "set_comments" => mock_batch_write_result(&arguments, "items"),
        "set_type" => mock_set_type_result(&arguments),
        "declare_type" => mock_declare_type(&arguments),
        "inspect_item" => mock_inspect_item(&arguments),
        "force_recompile" => mock_force_recompile(&arguments),
        "idb_save" => mock_idb_save(&arguments),
        "py_eval" => mock_py_eval(&arguments),
        "find_bytes" => mock_find_bytes(&arguments)?,
        "search_text" => mock_search_text(&arguments)?,
        "query_xrefs" => mock_query_xrefs(&arguments)?,
        "query_funcs" => mock_query_funcs(&arguments)?,
        "query_entities" => mock_query_entities(&arguments)?,
        other => {
            return Ok(WorkerResponse::Failed {
                code: "reverse_core_failed".to_string(),
                message: format!("unsupported IDA Core Function `{other}`"),
                writes: Vec::new(),
            });
        }
    };
    Ok(WorkerResponse::ReverseCoreFunction {
        result: ReverseCoreFunctionResult {
            function,
            result,
            warnings: Vec::new(),
        },
        writes: Vec::new(),
    })
}

fn mock_lookup_funcs(arguments: &Value) -> Result<Value, ServiceError> {
    let queries = normalize_core_list(arguments.get("queries").unwrap_or(&Value::Null));
    let runtime_module_base = optional_u64_argument(arguments, "runtime_module_base")?.unwrap_or(0);
    let ida_image_base = optional_u64_argument(arguments, "ida_image_base")?.unwrap_or(0);
    let mut items = Vec::new();
    for query in queries {
        if let Some(address) = parse_optional_u64_text(&query) {
            if address < runtime_module_base {
                return Err(ServiceError::Rpc(
                    "runtime address is below runtime_module_base".to_string(),
                ));
            }
            let rva = address - runtime_module_base;
            let ida_ea = ida_image_base
                .checked_add(rva)
                .ok_or_else(|| ServiceError::Rpc("ida_ea overflow".to_string()))?;
            let start = ida_ea & !0xff;
            items.push(json!({
                "query": query,
                "input_type": "address",
                "found": true,
                "runtime_address": address,
                "runtime_module_base": runtime_module_base,
                "rva": rva,
                "ida_image_base": ida_image_base,
                "ida_ea": ida_ea,
                "function_start": start,
                "function_end": start + 0x100,
                "function_name": format!("mock_function_{start:x}")
            }));
        } else {
            let found = !query.contains("missing");
            items.push(json!({
                "query": query,
                "input_type": "name",
                "found": found,
                "function_start": if found { json!(0x140001000u64) } else { Value::Null },
                "function_end": if found { json!(0x140001100u64) } else { Value::Null },
                "function_name": if found { json!(query) } else { Value::Null }
            }));
        }
    }
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

fn mock_int_convert(arguments: &Value) -> Value {
    let inputs = normalize_core_list(arguments.get("inputs").unwrap_or(arguments));
    let items: Vec<Value> = inputs
        .into_iter()
        .map(|input| match parse_core_integer(&input) {
            Some(value) => json!({
                "input": input,
                "decimal": value.to_string(),
                "hex": format!("0x{value:x}"),
                "binary": format!("0b{value:b}"),
                "bytes_le": value.to_le_bytes().to_vec(),
                "ascii": ascii_from_core_integer(value)
            }),
            None => json!({
                "input": input,
                "error": "not a supported integer, hex, binary, bytes, or ASCII representation"
            }),
        })
        .collect();
    let count = items.len();
    json!({ "items": items, "count": count })
}

fn mock_list_funcs(arguments: &Value) -> Result<Value, ServiceError> {
    let rows = vec![
        json!({ "address": 0x140001000u64, "name": "main", "size": 0x80 }),
        json!({ "address": 0x140001100u64, "name": "parse_args", "size": 0x60 }),
        json!({ "address": 0x140001200u64, "name": "dispatch_command", "size": 0xa0 }),
        json!({ "address": 0x140001300u64, "name": "cleanup", "size": 0x40 }),
    ];
    paginate_filtered(rows, arguments)
}

fn mock_list_globals(arguments: &Value) -> Result<Value, ServiceError> {
    let rows = vec![
        json!({ "address": 0x140020000u64, "name": "g_runtime_config", "type": "struct RuntimeConfig" }),
        json!({ "address": 0x140020080u64, "name": "g_worker_state", "type": "struct WorkerState" }),
    ];
    paginate_filtered(rows, arguments)
}

fn mock_list_imports(arguments: &Value) -> Result<Value, ServiceError> {
    let rows = vec![
        json!({ "module": "KERNEL32.dll", "name": "CreateFileW", "ordinal": Value::Null, "iat_ea": 0x140030000u64 }),
        json!({ "module": "KERNEL32.dll", "name": "ReadFile", "ordinal": Value::Null, "iat_ea": 0x140030008u64 }),
        json!({ "module": "USER32.dll", "name": "MessageBoxW", "ordinal": Value::Null, "iat_ea": 0x140030010u64 }),
    ];
    paginate_filtered(rows, arguments)
}

fn mock_list_strings(arguments: &Value) -> Result<Value, ServiceError> {
    let rows = vec![
        json!({ "address": 0x140040000u64, "length": 18, "type": 0, "text": "https://example.test" }),
        json!({ "address": 0x140040020u64, "length": 13, "type": 0, "text": "config_path" }),
        json!({ "address": 0x140040040u64, "length": 17, "type": 0, "text": "CreateFileW failed" }),
    ];
    paginate_filtered(rows, arguments)
}

fn mock_get_string(arguments: &Value) -> Result<Value, ServiceError> {
    let addr = required_u64_argument(arguments, "addr")?;
    let length = optional_u64_argument(arguments, "length")?;
    if let Some(length) = length
        && (length == 0 || length > 4096)
    {
        return Err(ServiceError::Rpc(
            "length must be between 1 and 4096 bytes".to_string(),
        ));
    }
    let string_type = optional_u64_argument(arguments, "type")?.unwrap_or(0);
    let text = match addr {
        0x140040000 => Some("https://example.test"),
        0x140040020 => Some("config_path"),
        0x140040040 => Some("CreateFileW failed"),
        _ => None,
    };
    Ok(json!({
        "address": addr,
        "found": text.is_some(),
        "length": length.unwrap_or_else(|| text.map_or(0, str::len) as u64),
        "type": string_type,
        "text": text
    }))
}

fn mock_get_bytes(arguments: &Value) -> Result<Value, ServiceError> {
    let addr = required_u64_argument(arguments, "addr")?;
    let length = required_u64_argument(arguments, "length")?;
    let bytes = mock_idb_bytes(addr, length)?;
    Ok(json!({
        "address": addr,
        "requested_length": length,
        "read_length": bytes.len(),
        "complete": bytes.len() as u64 == length,
        "bytes_hex": hex_encode(&bytes)
    }))
}

fn mock_get_int(arguments: &Value) -> Result<Value, ServiceError> {
    let addr = required_u64_argument(arguments, "addr")?;
    let size = optional_u64_argument(arguments, "size")?.unwrap_or(8);
    if !matches!(size, 1 | 2 | 4 | 8) {
        return Err(ServiceError::Rpc(
            "size must be one of 1, 2, 4, or 8 bytes".to_string(),
        ));
    }
    let endian = arguments
        .get("endian")
        .and_then(Value::as_str)
        .unwrap_or("little");
    if !matches!(endian, "little" | "big") {
        return Err(ServiceError::Rpc(
            "endian must be `little` or `big`".to_string(),
        ));
    }
    let bytes = mock_idb_bytes(addr, size)?;
    let mut value = 0u64;
    if endian == "little" {
        for (index, byte) in bytes.iter().enumerate() {
            value |= (*byte as u64) << (index * 8);
        }
    } else {
        for byte in &bytes {
            value = (value << 8) | (*byte as u64);
        }
    }
    Ok(json!({
        "address": addr,
        "size": size,
        "endian": endian,
        "complete": true,
        "bytes_hex": hex_encode(&bytes),
        "decimal": value.to_string(),
        "hex": format!("0x{value:x}")
    }))
}

fn mock_decompile(arguments: &Value) -> Result<Value, ServiceError> {
    let addr = required_u64_argument(arguments, "addr")?;
    let start = addr & !0xff;
    Ok(json!({
        "addr": addr,
        "function_start": start,
        "function_name": format!("mock_function_{start:x}"),
        "language": "c",
        "pseudocode": format!("int mock_function_{start:x}(void) {{\n    return 0;\n}}")
    }))
}

fn mock_disasm(arguments: &Value) -> Result<Value, ServiceError> {
    let addr = required_u64_argument(arguments, "addr")?;
    let start = addr & !0xff;
    Ok(json!({
        "addr": addr,
        "function_start": start,
        "function_name": format!("mock_function_{start:x}"),
        "arguments": [{ "name": "arg_0", "location": "rcx", "type": "uint64_t" }],
        "stack_frame": { "size": 0x40, "locals": [{ "name": "var_8", "offset": -8, "size": 8 }] },
        "instructions": [
            { "ea": start, "text": "push rbp" },
            { "ea": start + 1, "text": "mov rbp, rsp" },
            { "ea": start + 4, "text": "ret" }
        ]
    }))
}

fn mock_xrefs_to(arguments: &Value) -> Result<Value, ServiceError> {
    let addrs = normalize_core_list(arguments.get("addrs").unwrap_or(&Value::Null));
    let mut items = Vec::new();
    for addr in addrs {
        let ea = parse_optional_u64_text(&addr)
            .ok_or_else(|| ServiceError::Rpc("xrefs_to addrs must be addresses".to_string()))?;
        items.push(json!({
            "to": ea,
            "xrefs": [
                { "from": ea.saturating_sub(0x20), "type": "code", "function": format!("mock_function_{:x}", ea.saturating_sub(0x20) & !0xff) }
            ]
        }));
    }
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

fn mock_xrefs_to_field(arguments: &Value) -> Result<Value, ServiceError> {
    let queries = normalize_core_list(arguments.get("queries").unwrap_or(&Value::Null));
    let items: Vec<Value> = queries
        .into_iter()
        .map(|query| {
            json!({
                "query": query,
                "xrefs": [
                    { "from": 0x140001240u64, "operand": 1, "access": "read" }
                ]
            })
        })
        .collect();
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

fn mock_callees(arguments: &Value) -> Result<Value, ServiceError> {
    let addrs = normalize_core_list(arguments.get("addrs").unwrap_or(&Value::Null));
    let mut items = Vec::new();
    for addr in addrs {
        let ea = parse_optional_u64_text(&addr)
            .ok_or_else(|| ServiceError::Rpc("callees addrs must be addresses".to_string()))?;
        items.push(json!({
            "function": ea,
            "callees": [
                { "ea": 0x140001100u64, "name": "parse_args" },
                { "ea": 0x140001200u64, "name": "dispatch_command" }
            ]
        }));
    }
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

fn mock_batch_write_result(arguments: &Value, field: &'static str) -> Value {
    let items = arguments
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            arguments
                .get(field)
                .map(|value| vec![value.clone()])
                .unwrap_or_default()
        });
    let results: Vec<Value> = items
        .into_iter()
        .map(|item| {
            json!({
                "input": item,
                "ok": true
            })
        })
        .collect();
    let count = results.len();
    json!({
        "items": results,
        "count": count,
        "changed_count": count
    })
}

fn mock_batch_write_result_with_diagnostics(arguments: &Value, field: &'static str) -> Value {
    let items = arguments
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            arguments
                .get(field)
                .map(|value| vec![value.clone()])
                .unwrap_or_default()
        });
    let results: Vec<Value> = items
        .into_iter()
        .map(|item| {
            let addr = mock_item_addr(&item);
            json!({
                "input": item,
                "ea": addr,
                "ok": true,
                "item_state": mock_item_state(addr)
            })
        })
        .collect();
    let count = results.len();
    json!({
        "items": results,
        "count": count,
        "changed_count": count
    })
}

fn mock_set_type_result(arguments: &Value) -> Value {
    let items = arguments
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            arguments
                .get("items")
                .map(|value| vec![value.clone()])
                .unwrap_or_default()
        });
    let results: Vec<Value> = items
        .into_iter()
        .map(|item| {
            let addr = mock_item_addr(&item);
            let type_text = item.get("type").and_then(Value::as_str).unwrap_or_default();
            json!({
                "input": item,
                "ea": addr,
                "ok": true,
                "generated_decl": mock_generated_decl(type_text),
                "item_state": mock_item_state(addr)
            })
        })
        .collect();
    let count = results.len();
    json!({
        "items": results,
        "count": count,
        "changed_count": count
    })
}

fn mock_item_addr(item: &Value) -> u64 {
    item.get("addr")
        .and_then(parse_u64_value)
        .unwrap_or(0x140020000)
}

fn mock_item_state(addr: u64) -> Value {
    json!({
        "ea": addr,
        "item_head": addr,
        "item_end": addr + 8,
        "item_size": 8,
        "is_item_head": true,
        "name": "mock_symbol",
        "head_name": "mock_symbol",
        "is_code": false,
        "is_data": true
    })
}

fn mock_generated_decl(type_text: &str) -> String {
    let trimmed = type_text.trim();
    if let Some(array_pos) = trimmed.find('[') {
        let (before_array, array_suffix) = trimmed.split_at(array_pos);
        format!("{} mock_symbol{};", before_array.trim(), array_suffix)
    } else {
        format!("{trimmed} mock_symbol;")
    }
}

fn mock_declare_type(arguments: &Value) -> Value {
    let decls = normalize_core_list(arguments.get("decls").unwrap_or(&Value::Null));
    let items: Vec<Value> = decls
        .iter()
        .enumerate()
        .map(|(index, decl)| {
            let ok = !decl.to_ascii_lowercase().contains("invalid");
            let errors = if ok { 0 } else { 1 };
            let mut item = json!({
                "index": index,
                "ok": ok,
                "errors": errors,
                "decl": decl
            });
            if !ok {
                item["hint"] = json!(
                    "IDA failed to parse this declaration; try declaring dependent typedefs separately or remove unsupported calling-convention syntax"
                );
            }
            item
        })
        .collect();
    let errors = items
        .iter()
        .filter(|item| !item["ok"].as_bool().unwrap_or(false))
        .count();
    let changed_count = items.len().saturating_sub(errors);
    json!({
        "ok": errors == 0,
        "count": decls.len(),
        "changed_count": changed_count,
        "errors": errors,
        "items": items
    })
}

fn mock_inspect_item(arguments: &Value) -> Value {
    let queries = normalize_core_list(arguments.get("queries").unwrap_or(&Value::Null));
    let items: Vec<Value> = queries
        .into_iter()
        .map(|query| {
            let addr = parse_optional_u64_text(&query).unwrap_or(0x140020000);
            json!({
                "query": query,
                "ea": addr,
                "item_state": mock_item_state(addr)
            })
        })
        .collect();
    let count = items.len();
    json!({
        "items": items,
        "count": count
    })
}

fn mock_force_recompile(arguments: &Value) -> Value {
    let addrs = normalize_core_list(arguments.get("addrs").unwrap_or(&Value::Null));
    let items: Vec<Value> = addrs
        .into_iter()
        .map(|addr| json!({ "query": addr, "ea": 0x140001000u64, "ok": true }))
        .collect();
    let count = items.len();
    json!({
        "items": items,
        "count": count,
        "changed_count": count,
        "all": count == 0
    })
}

fn mock_idb_save(arguments: &Value) -> Value {
    json!({
        "ok": true,
        "path": arguments.get("path").cloned().unwrap_or(Value::Null),
        "changed_count": 1
    })
}

fn mock_py_eval(arguments: &Value) -> Value {
    let code = arguments
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "ok": true,
        "stdout": if code.contains("print") { "mock py_eval\n" } else { "" },
        "stderr": "",
        "error": Value::Null
    })
}

fn mock_find_bytes(arguments: &Value) -> Result<Value, ServiceError> {
    let patterns = normalize_core_list(arguments.get("patterns").unwrap_or(&Value::Null));
    let rows: Vec<Value> = patterns
        .into_iter()
        .enumerate()
        .map(|(index, pattern)| {
            json!({
                "pattern": pattern,
                "ea": 0x140050000u64 + index as u64
            })
        })
        .collect();
    paginate_filtered(rows, arguments)
}

fn mock_search_text(arguments: &Value) -> Result<Value, ServiceError> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let rows = vec![
        json!({ "scope": "strings", "ea": 0x140040020u64, "text": "config_path" }),
        json!({ "scope": "names", "ea": 0x140001100u64, "text": "parse_args" }),
        json!({ "scope": "comments", "ea": 0x140001200u64, "text": "dispatch command handler" }),
    ]
    .into_iter()
    .filter(|row| query.is_empty() || row.to_string().to_ascii_lowercase().contains(&query))
    .collect();
    paginate_filtered(rows, arguments)
}

fn mock_query_xrefs(arguments: &Value) -> Result<Value, ServiceError> {
    let target = arguments.get("target").cloned().unwrap_or(Value::Null);
    let rows = vec![
        json!({
            "direction": arguments.get("direction").and_then(Value::as_str).unwrap_or("to"),
            "type": "code",
            "from": 0x140001220u64,
            "to": target.clone(),
            "function": { "address": 0x140001200u64, "name": "dispatch_command" }
        }),
        json!({
            "direction": arguments.get("direction").and_then(Value::as_str).unwrap_or("to"),
            "type": "data",
            "from": 0x140020000u64,
            "to": target,
            "function": Value::Null
        }),
    ];
    paginate_filtered(rows, arguments)
}

fn mock_query_funcs(arguments: &Value) -> Result<Value, ServiceError> {
    mock_list_funcs(arguments)
}

fn mock_query_entities(arguments: &Value) -> Result<Value, ServiceError> {
    match arguments
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("functions")
    {
        "functions" => mock_list_funcs(arguments),
        "globals" | "names" => mock_list_globals(arguments),
        "imports" => mock_list_imports(arguments),
        "strings" => mock_list_strings(arguments),
        other => Err(ServiceError::Rpc(format!(
            "unsupported query_entities kind `{other}`"
        ))),
    }
}

fn mock_idb_bytes(addr: u64, length: u64) -> Result<Vec<u8>, ServiceError> {
    if length == 0 || length > 4096 {
        return Err(ServiceError::Rpc(
            "length must be between 1 and 4096 bytes".to_string(),
        ));
    }
    Ok((0..length)
        .map(|offset| addr.wrapping_add(offset) as u8)
        .collect())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

fn paginate_filtered(rows: Vec<Value>, arguments: &Value) -> Result<Value, ServiceError> {
    let offset = optional_u64_argument(arguments, "offset")?.unwrap_or(0) as usize;
    let count = optional_u64_argument(arguments, "count")?
        .or(optional_u64_argument(arguments, "limit")?)
        .unwrap_or(50)
        .min(1_000) as usize;
    let filter = arguments
        .get("filter")
        .or_else(|| arguments.get("query"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let filtered: Vec<Value> = rows
        .into_iter()
        .filter(|row| {
            filter
                .as_ref()
                .is_none_or(|needle| row.to_string().to_ascii_lowercase().contains(needle))
        })
        .collect();
    let total = filtered.len();
    let items: Vec<Value> = filtered.into_iter().skip(offset).take(count).collect();
    Ok(json!({
        "offset": offset,
        "count": items.len(),
        "total": total,
        "items": items
    }))
}

fn normalize_core_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.trim().to_string(),
                other => other.to_string(),
            })
            .filter(|item| !item.is_empty())
            .collect(),
        Value::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        Value::Null => Vec::new(),
        other => vec![other.to_string()],
    }
}

fn required_u64_argument(arguments: &Value, field: &'static str) -> Result<u64, ServiceError> {
    arguments
        .get(field)
        .and_then(parse_u64_value)
        .ok_or_else(|| ServiceError::Rpc(format!("{field} must be an address")))
}

fn optional_u64_argument(
    arguments: &Value,
    field: &'static str,
) -> Result<Option<u64>, ServiceError> {
    match arguments.get(field) {
        Some(value) => parse_u64_value(value)
            .map(Some)
            .ok_or_else(|| ServiceError::Rpc(format!("{field} must be an unsigned integer"))),
        None => Ok(None),
    }
}

fn parse_u64_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => parse_optional_u64_text(text),
        _ => None,
    }
}

fn parse_optional_u64_text(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(binary) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        u64::from_str_radix(binary, 2).ok()
    } else {
        text.parse::<u64>().ok()
    }
}

fn parse_core_integer(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(bytes) = text
        .strip_prefix("bytes:")
        .or_else(|| text.strip_prefix("bytes_le:"))
    {
        let mut value = 0u64;
        for (index, byte) in bytes
            .split([' ', ',', '-'])
            .filter(|part| !part.is_empty())
            .take(8)
            .enumerate()
        {
            let byte = u8::from_str_radix(byte.trim_start_matches("0x"), 16).ok()?;
            value |= (byte as u64) << (index * 8);
        }
        Some(value)
    } else if let Some(ascii) = text.strip_prefix("ascii:") {
        let mut value = 0u64;
        for (index, byte) in ascii.as_bytes().iter().take(8).enumerate() {
            value |= (*byte as u64) << (index * 8);
        }
        Some(value)
    } else {
        parse_optional_u64_text(text)
    }
}

fn ascii_from_core_integer(value: u64) -> String {
    value
        .to_le_bytes()
        .into_iter()
        .take_while(|byte| *byte != 0)
        .map(|byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                byte as char
            } else {
                '.'
            }
        })
        .collect()
}

fn write_mock_eval_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    command: String,
    artifact_dir: PathBuf,
) -> Result<WorkerResponse, ServiceError> {
    let raw_relative_path = session_relative_path(
        &session_id,
        &format!("raw/{}.txt", operation_id.id.as_str()),
    );
    let transcript_relative_path = session_relative_path(&session_id, "transcript.log");
    let events_relative_path = session_relative_path(&session_id, "events.jsonl");
    let raw_path = artifact_dir
        .join("raw")
        .join(format!("{}.txt", operation_id.id.as_str()));
    let transcript_path = artifact_dir.join("transcript.log");
    let events_path = artifact_dir.join("events.jsonl");
    if let Some(parent) = raw_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let output = if command == ".mock_long_output" {
        "x".repeat(DEFAULT_INLINE_TEXT_BYTE_LIMIT + 100)
    } else {
        format!(
            "mock debug worker accepted eval command; real DbgEng execution is not wired yet\ncommand: {}\n",
            command
        )
    };
    fs::write(&raw_path, &output)?;
    let transcript = format!("> {}\n{}\n", command, output);
    let mut transcript_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript_path)?;
    transcript_file.write_all(transcript.as_bytes())?;
    let mut events_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)?;
    serde_json::to_writer(
        &mut events_file,
        &json!({
            "event": "output",
            "session_id": session_id,
            "operation_id": operation_id,
            "command": command,
            "timestamp": Timestamp::now(),
            "byte_len": output.len(),
        }),
    )?;
    events_file.write_all(b"\n")?;
    let preview = inline_text_preview(&output, DEFAULT_INLINE_TEXT_BYTE_LIMIT);
    let mut warnings = vec!["mock worker: real DbgEng execution is not wired yet".to_string()];
    if preview.truncated {
        warnings.push(format!(
            "output truncated to {} bytes inline; full output saved to raw output artifact",
            preview.inline_byte_limit
        ));
    }

    Ok(WorkerResponse::DebugCommand {
        result: DebugCommandResult {
            session_id,
            operation_id: None,
            command,
            output: preview.text,
            output_truncated: preview.truncated,
            full_output_byte_len: Some(preview.full_byte_len),
            inline_output_byte_limit: Some(preview.inline_byte_limit),
            final_state: Some(DebugSessionState::Ready),
            raw_output: None,
            full_output_artifact_ref: None,
            warnings,
            error: None,
        },
        writes: vec![
            WorkerArtifactWrite {
                relative_path: raw_relative_path,
                kind: "debug.raw_output".to_string(),
                byte_len: output.len() as u64,
                description: Some("mock debug eval raw output".to_string()),
            },
            WorkerArtifactWrite {
                relative_path: transcript_relative_path,
                kind: "debug.transcript".to_string(),
                byte_len: transcript.len() as u64,
                description: Some("debug session transcript".to_string()),
            },
            WorkerArtifactWrite {
                relative_path: events_relative_path,
                kind: "debug.events".to_string(),
                byte_len: output.len() as u64,
                description: Some("debug session events".to_string()),
            },
        ],
    })
}

fn write_mock_memory_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    address: u64,
    length: u64,
    artifact_dir: PathBuf,
) -> Result<WorkerResponse, ServiceError> {
    let length = usize::try_from(length)
        .map_err(|_| ServiceError::Worker("memory read length is too large".to_string()))?;
    let relative_path = session_relative_path(
        &session_id,
        &format!("memory/{}.bin", operation_id.id.as_str()),
    );
    let memory_path = artifact_dir
        .join("memory")
        .join(format!("{}.bin", operation_id.id.as_str()));
    if let Some(parent) = memory_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = vec![0u8; length];
    fs::write(&memory_path, &bytes)?;

    Ok(WorkerResponse::DebugMemory {
        result: DebugMemoryResult {
            session_id,
            operation_id: None,
            address,
            requested_length: length as u64,
            bytes_read: length as u64,
            memory: None,
            warnings: vec!["mock worker: real DbgEng memory read is not wired yet".to_string()],
            error: None,
        },
        writes: vec![WorkerArtifactWrite {
            relative_path,
            kind: "debug.memory".to_string(),
            byte_len: length as u64,
            description: Some("mock debug memory read".to_string()),
        }],
    })
}

struct RegisteredWorkerWrites {
    artifacts: Vec<ArtifactRef>,
    raw_output: Option<ArtifactRef>,
    raw_output_byte_len: Option<u64>,
    memory: Option<ArtifactRef>,
}

fn finalize_debug_command_result(result: &mut DebugCommandResult, writes: &RegisteredWorkerWrites) {
    result.raw_output = writes.raw_output.clone();
    if result.full_output_artifact_ref.is_none() {
        result.full_output_artifact_ref = writes.raw_output.clone();
    }
    let full_output_byte_len = result
        .full_output_byte_len
        .or(writes.raw_output_byte_len)
        .unwrap_or(result.output.len() as u64);
    result.full_output_byte_len = Some(full_output_byte_len);
    result
        .inline_output_byte_limit
        .get_or_insert(DEFAULT_INLINE_TEXT_BYTE_LIMIT as u64);

    if result.output.len() > DEFAULT_INLINE_TEXT_BYTE_LIMIT {
        let preview = inline_text_preview(&result.output, DEFAULT_INLINE_TEXT_BYTE_LIMIT);
        result.output = preview.text;
        result.output_truncated = true;
    }
    if full_output_byte_len > DEFAULT_INLINE_TEXT_BYTE_LIMIT as u64 {
        result.output_truncated = true;
    }
    if result.output_truncated
        && !result
            .warnings
            .iter()
            .any(|warning| warning.contains("output truncated"))
    {
        result.warnings.push(format!(
            "output truncated to {} bytes inline; full output saved to raw output artifact",
            DEFAULT_INLINE_TEXT_BYTE_LIMIT
        ));
    }
}

fn command_result_response(
    result: &DebugCommandResult,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    let mut value = serde_json::to_value(result)?;
    add_operation_refs(&mut value, status, writes, None)?;
    Ok(value)
}

fn memory_result_response(
    result: &DebugMemoryResult,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    let mut value = serde_json::to_value(result)?;
    add_operation_refs(&mut value, status, writes, writes.memory.as_ref())?;
    Ok(value)
}

fn add_operation_refs(
    value: &mut Value,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
    memory_ref: Option<&ArtifactRef>,
) -> Result<(), ServiceError> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    object.insert(
        "operation_status".to_string(),
        serde_json::to_value(&status)?,
    );
    object.insert(
        "artifact_refs".to_string(),
        serde_json::to_value(&writes.artifacts)?,
    );
    object.insert(
        "raw_output_ref".to_string(),
        serde_json::to_value(&writes.raw_output)?,
    );
    object.insert(
        "operation".to_string(),
        json!({
            "status": status,
            "artifact_refs": writes.artifacts,
            "raw_output_ref": writes.raw_output,
        }),
    );
    if let Some(memory_ref) = memory_ref {
        object.insert("memory_ref".to_string(), serde_json::to_value(memory_ref)?);
    }
    Ok(())
}

fn register_worker_writes(
    workspace: &Workspace,
    operation_id: &OperationRef,
    writes: &[WorkerArtifactWrite],
) -> Result<RegisteredWorkerWrites, ServiceError> {
    let mut artifacts = Vec::new();
    let mut raw_output = None;
    let mut raw_output_byte_len = None;
    let mut memory = None;
    for write in writes {
        let artifact_id = next_artifact_ref();
        if write.kind == "debug.raw_output" {
            raw_output = Some(artifact_id.clone());
            raw_output_byte_len = Some(write.byte_len);
        }
        if write.kind == "debug.memory" {
            memory = Some(artifact_id.clone());
        }
        workspace.register_artifact(&ArtifactMetadata {
            artifact_id: artifact_id.clone(),
            kind: write.kind.clone(),
            relative_path: write.relative_path.clone(),
            created_at: Timestamp::now(),
            operation_id: Some(operation_id.clone()),
            byte_len: Some(write.byte_len),
            description: write.description.clone(),
        })?;
        artifacts.push(artifact_id);
    }
    Ok(RegisteredWorkerWrites {
        artifacts,
        raw_output,
        raw_output_byte_len,
        memory,
    })
}

fn unique_pipe_name(session_id: &SessionRef) -> String {
    let counter = WORKER_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
        ^ counter as u128;
    format!(
        r"\\.\pipe\dbgatlas-{}-{}-{nonce:x}",
        std::process::id(),
        session_id.id.as_str()
    )
}

fn worker_executable_path() -> Result<PathBuf, ServiceError> {
    let current = std::env::current_exe()?;
    let directory = current
        .parent()
        .ok_or_else(|| ServiceError::Worker("current executable has no parent".to_string()))?;
    let file_name = if cfg!(windows) {
        "dbgatlas-worker.exe"
    } else {
        "dbgatlas-worker"
    };
    Ok(directory.join(file_name))
}

fn prepare_dbgeng_dirs_for_worker_identity(
    identity: &WorkerIdentity,
    dbgeng_dirs: &[PathBuf],
) -> Result<Vec<PathBuf>, ServiceError> {
    #[cfg(windows)]
    {
        if matches!(identity, WorkerIdentity::ActiveInteractiveUser) {
            return prepare_active_user_dbgeng_dirs(dbgeng_dirs, &active_user_dbgeng_cache_root());
        }
    }

    Ok(dbgeng_dirs.to_vec())
}

#[cfg(windows)]
fn prepare_active_user_dbgeng_dirs(
    dbgeng_dirs: &[PathBuf],
    cache_root: &Path,
) -> Result<Vec<PathBuf>, ServiceError> {
    dbgeng_dirs
        .iter()
        .map(|dir| {
            if is_windowsapps_windbg_dbgeng_dir(dir) {
                match cache_windowsapps_dbgeng_dir_for_active_user(dir, cache_root) {
                    Ok(cached) => Ok(cached),
                    Err(error) => {
                        append_service_diagnostic_log(&format!(
                            "dbgeng_runtime_cache_failed source={} error={}",
                            sanitize_log_value(&dir.display().to_string()),
                            sanitize_log_value(&error.to_string())
                        ));
                        Ok(dir.clone())
                    }
                }
            } else {
                Ok(dir.clone())
            }
        })
        .collect()
}

#[cfg(windows)]
fn active_user_dbgeng_cache_root() -> PathBuf {
    default_windows_service_paths()
        .var_dir
        .join("runtime-cache")
        .join("dbgeng")
}

#[cfg(windows)]
fn is_windowsapps_windbg_dbgeng_dir(path: &Path) -> bool {
    let path = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    path.contains("\\windowsapps\\microsoft.windbg_")
}

#[cfg(windows)]
fn cache_windowsapps_dbgeng_dir_for_active_user(
    source_dir: &Path,
    cache_root: &Path,
) -> Result<PathBuf, ServiceError> {
    let key = dbgeng_cache_key(source_dir);
    let destination = cache_root.join(key);
    let marker = destination.join(".dbgatlas-cache-complete");
    if marker.is_file() && destination.join("dbgeng.dll").is_file() {
        return Ok(destination);
    }

    fs::create_dir_all(cache_root)?;
    let staging_nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let staging = cache_root.join(format!(
        "{}.staging-{}-{staging_nonce:x}",
        destination
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("dbgeng"),
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }

    copy_dir_recursive(source_dir, &staging)?;
    fs::write(
        staging.join(".dbgatlas-cache-complete"),
        source_dir.display().to_string(),
    )?;
    match fs::rename(&staging, &destination) {
        Ok(()) => {}
        Err(_error) if marker.is_file() && destination.join("dbgeng.dll").is_file() => {
            let _ = fs::remove_dir_all(&staging);
            append_service_diagnostic_log(&format!(
                "dbgeng_runtime_cache_race_reused destination={}",
                sanitize_log_value(&destination.display().to_string())
            ));
            return Ok(destination);
        }
        Err(error) => return Err(error.into()),
    }
    append_service_diagnostic_log(&format!(
        "dbgeng_runtime_cached source={} destination={}",
        sanitize_log_value(&source_dir.display().to_string()),
        sanitize_log_value(&destination.display().to_string())
    ));
    Ok(destination)
}

#[cfg(windows)]
fn dbgeng_cache_key(source_dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    source_dir
        .to_string_lossy()
        .to_ascii_lowercase()
        .hash(&mut hasher);
    let hash = hasher.finish();
    let label = source_dir
        .parent()
        .and_then(Path::file_name)
        .or_else(|| source_dir.file_name())
        .and_then(OsStr::to_str)
        .map(sanitize_cache_label)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| "dbgeng".to_string());
    format!("{label}-{hash:016x}")
}

#[cfg(windows)]
fn sanitize_cache_label(label: &str) -> String {
    label
        .chars()
        .take(96)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(windows)]
fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), ServiceError> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn windbg_runtime_source_candidates(paths: &ServiceInstallPaths) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    push_unique_path(&mut candidates, paths.windbg_runtime_dir.clone());
    push_unique_path(&mut candidates, paths.legacy_windbg_runtime_dir());
    candidates
}

#[cfg(windows)]
fn append_windbg_runtime_source_candidates(
    candidates: &mut Vec<PathBuf>,
    paths: &ServiceInstallPaths,
) {
    for candidate in windbg_runtime_source_candidates(paths) {
        push_unique_path(candidates, candidate);
    }
}

#[cfg(windows)]
fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

#[cfg(windows)]
fn copy_existing_windbg_runtime_to_destination(
    candidates: &[PathBuf],
    destination_dir: &Path,
) -> Result<Option<PathBuf>, ServiceError> {
    for source_dir in candidates {
        if source_dir == destination_dir || !source_dir.is_dir() {
            continue;
        }
        if destination_dir.exists() {
            fs::remove_dir_all(destination_dir)?;
        }
        copy_dir_recursive(source_dir, destination_dir)?;
        match validate_windbg_runtime_dir(destination_dir) {
            Ok(()) => {
                append_service_diagnostic_log(&format!(
                    "windbg_runtime_preserved source={} destination={}",
                    sanitize_log_value(&source_dir.display().to_string()),
                    sanitize_log_value(&destination_dir.display().to_string())
                ));
                return Ok(Some(source_dir.clone()));
            }
            Err(error) => {
                let _ = fs::remove_dir_all(destination_dir);
                append_service_diagnostic_log(&format!(
                    "windbg_runtime_preserve_incomplete source={} destination={} error={}",
                    sanitize_log_value(&source_dir.display().to_string()),
                    sanitize_log_value(&destination_dir.display().to_string()),
                    sanitize_log_value(&error.to_string())
                ));
            }
        }
    }
    Ok(None)
}

#[cfg(windows)]
#[derive(Debug)]
struct StagedWindbgRuntime {
    source_dir: PathBuf,
    staging_dir: PathBuf,
    destination_dir: PathBuf,
}

#[cfg(windows)]
fn prepare_store_windbg_runtime_staging(
    paths: &ServiceInstallPaths,
    suffix: &str,
) -> Result<Option<StagedWindbgRuntime>, ServiceError> {
    prepare_store_windbg_runtime_staging_for_destination(&paths.windbg_runtime_dir, suffix)
}

#[cfg(windows)]
fn prepare_store_windbg_runtime_staging_for_destination(
    destination_dir: &Path,
    suffix: &str,
) -> Result<Option<StagedWindbgRuntime>, ServiceError> {
    let Some(source_dir) = resolve_store_windbg_dbgeng_dir() else {
        append_service_diagnostic_log("windbg_runtime_source_not_found");
        return Ok(None);
    };
    if let Err(error) = validate_windbg_runtime_dir(&source_dir) {
        append_service_diagnostic_log(&format!(
            "windbg_runtime_source_incomplete source={} error={}",
            sanitize_log_value(&source_dir.display().to_string()),
            sanitize_log_value(&error.to_string())
        ));
        return Ok(None);
    }
    let parent = destination_dir.parent().ok_or_else(|| {
        ServiceError::ServiceControl(format!(
            "WinDbg runtime destination has no parent: {}",
            destination_dir.display()
        ))
    })?;
    let staging_dir = parent.join(format!("{}.next-{suffix}", windbg_runtime_arch()));
    stage_windbg_runtime_from_source(&source_dir, destination_dir, &staging_dir).map(Some)
}

#[cfg(windows)]
fn prepare_update_windbg_runtime_in_staging(
    paths: &ServiceInstallPaths,
    staging_bin_dir: &Path,
    suffix: &str,
) -> Result<Option<PathBuf>, ServiceError> {
    let destination_dir = staging_bin_dir
        .join(WINDOWS_SERVICE_RT_DIR)
        .join("windbg")
        .join(windbg_runtime_arch());
    if let Some(staged) =
        prepare_store_windbg_runtime_staging_for_destination(&destination_dir, suffix)?
    {
        activate_staged_windbg_runtime(&staged, suffix)?;
        return Ok(Some(destination_dir));
    }
    let candidates = windbg_runtime_source_candidates(paths);
    if copy_existing_windbg_runtime_to_destination(&candidates, &destination_dir)?.is_some() {
        return Ok(Some(destination_dir));
    }
    Ok(None)
}

#[cfg(windows)]
fn stage_windbg_runtime_from_source(
    source_dir: &Path,
    destination_dir: &Path,
    staging_dir: &Path,
) -> Result<StagedWindbgRuntime, ServiceError> {
    if staging_dir.exists() {
        fs::remove_dir_all(staging_dir)?;
    }
    if let Some(parent) = staging_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    copy_dir_recursive(source_dir, staging_dir)?;
    validate_windbg_runtime_dir(staging_dir)?;
    fs::write(
        staging_dir.join(".dbgatlas-runtime-source"),
        source_dir.display().to_string(),
    )?;
    append_service_diagnostic_log(&format!(
        "windbg_runtime_staged source={} staging={}",
        sanitize_log_value(&source_dir.display().to_string()),
        sanitize_log_value(&staging_dir.display().to_string())
    ));
    Ok(StagedWindbgRuntime {
        source_dir: source_dir.to_path_buf(),
        staging_dir: staging_dir.to_path_buf(),
        destination_dir: destination_dir.to_path_buf(),
    })
}

#[cfg(windows)]
fn activate_staged_windbg_runtime(
    staged: &StagedWindbgRuntime,
    suffix: &str,
) -> Result<(), ServiceError> {
    validate_windbg_runtime_dir(&staged.staging_dir)?;
    let parent = staged.destination_dir.parent().ok_or_else(|| {
        ServiceError::ServiceControl(format!(
            "WinDbg runtime destination has no parent: {}",
            staged.destination_dir.display()
        ))
    })?;
    fs::create_dir_all(parent)?;
    let old_dir = parent.join(format!("{}.old-{suffix}", windbg_runtime_arch()));
    if old_dir.exists() {
        fs::remove_dir_all(&old_dir)?;
    }
    if staged.destination_dir.exists() {
        fs::rename(&staged.destination_dir, &old_dir)?;
    }
    match fs::rename(&staged.staging_dir, &staged.destination_dir) {
        Ok(()) => {
            if old_dir.exists() {
                fs::remove_dir_all(&old_dir)?;
            }
            append_service_diagnostic_log(&format!(
                "windbg_runtime_activated source={} destination={}",
                sanitize_log_value(&staged.source_dir.display().to_string()),
                sanitize_log_value(&staged.destination_dir.display().to_string())
            ));
            Ok(())
        }
        Err(error) => {
            if old_dir.exists() && !staged.destination_dir.exists() {
                let _ = fs::rename(&old_dir, &staged.destination_dir);
            }
            Err(error.into())
        }
    }
}

#[cfg(windows)]
fn validate_windbg_runtime_dir(dir: &Path) -> Result<(), ServiceError> {
    let required: &[&[&str]] = &[
        &["dbgeng.dll"],
        &["dbghelp.dll"],
        &["dbgmodel.dll"],
        &["ttd", "TTD.exe"],
        &["ttd", "TTDInject.exe"],
        &["ttd", "TTDLoader.dll"],
        &["ttd", "TTDRecord.dll"],
        &["ttd", "TTDRecordCPU.dll"],
        &["ttd", "TTDReplay.dll"],
        &["ttd", "TTDReplayCPU.dll"],
    ];
    let missing = required
        .iter()
        .map(|parts| {
            parts
                .iter()
                .fold(dir.to_path_buf(), |path, part| path.join(part))
        })
        .filter(|path| !path.is_file())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(ServiceError::ServiceControl(format!(
            "WinDbg runtime is incomplete under {}: missing {}",
            dir.display(),
            missing.join(", ")
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn cleanup_windbg_runtime_update_dirs(paths: &ServiceInstallPaths) -> Result<(), ServiceError> {
    let windbg_dir = paths.rt_dir.join("windbg");
    if !windbg_dir.is_dir() {
        return Ok(());
    }
    let next_prefix = format!("{}.next-", windbg_runtime_arch());
    let old_prefix = format!("{}.old-", windbg_runtime_arch());
    for entry in fs::read_dir(&windbg_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if name.starts_with(&next_prefix) || name.starts_with(&old_prefix) {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

fn spawn_worker_process(
    worker_exe: &Path,
    pipe_name: &str,
    session_id: &str,
    identity: &WorkerIdentity,
    dbgeng_dirs: &[PathBuf],
) -> Result<WorkerProcess, ServiceError> {
    let args = worker_process_args(pipe_name, session_id, dbgeng_dirs);
    match identity {
        WorkerIdentity::ActiveInteractiveUser => {
            spawn_active_interactive_worker_process(worker_exe, &args)
        }
        WorkerIdentity::CurrentUserDevMode | WorkerIdentity::LocalSystem => Ok(WorkerProcess::Std(
            Command::new(worker_exe).args(&args).spawn()?,
        )),
    }
}

fn worker_process_args(pipe_name: &str, session_id: &str, dbgeng_dirs: &[PathBuf]) -> Vec<String> {
    let mut args = vec![
        "--pipe".to_string(),
        pipe_name.to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
    ];
    for dbgeng_dir in dbgeng_dirs {
        args.push("--dbgeng-dir".to_string());
        args.push(dbgeng_dir.display().to_string());
    }
    args
}

fn debug_target_kind(target: &DebugTarget) -> &'static str {
    match target {
        DebugTarget::File { path } if is_ttd_run_file(path) => "ttd_run_file",
        DebugTarget::File { .. } => "dump_file",
        DebugTarget::Attach { .. } => "attach",
        DebugTarget::Launch { .. } => "launch",
    }
}

fn debug_worker_dbgeng_attempts(
    target: &DebugTarget,
    dbgeng_dirs: &[PathBuf],
) -> Vec<Vec<PathBuf>> {
    // DbgEng DLL 一旦加载到进程内就不能可靠切换版本；TTD `.run` 又常常依赖
    // Store/SDK 版本差异。因此 `.run` 每个候选目录用独立 worker 尝试，
    // 普通 dump/attach 仍把候选一次性交给 worker，避免不必要的进程 churn。
    if matches!(target, DebugTarget::File { path } if is_ttd_run_file(path))
        && dbgeng_dirs.len() > 1
    {
        return dbgeng_dirs.iter().cloned().map(|dir| vec![dir]).collect();
    }
    vec![dbgeng_dirs.to_vec()]
}

fn is_ttd_run_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("run"))
}

#[cfg(windows)]
fn spawn_active_interactive_worker_process(
    worker_exe: &Path,
    args: &[String],
) -> Result<WorkerProcess, ServiceError> {
    windows_active_user_process::spawn(worker_exe, args).map(WorkerProcess::RawWindows)
}

#[cfg(not(windows))]
fn spawn_active_interactive_worker_process(
    _worker_exe: &Path,
    _args: &[String],
) -> Result<WorkerProcess, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsServicePayloadMode {
    Copy,
    UseExisting,
}

impl Default for WindowsServicePayloadMode {
    fn default() -> Self {
        Self::Copy
    }
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
struct PreparedServiceUpdate {
    source_dir: PathBuf,
    bin_payload: Vec<ServicePayloadFile>,
    config_payload: Option<ServicePayloadFile>,
    response: WindowsServiceUpdateAccepted,
}

pub fn default_windows_service_paths() -> ServiceInstallPaths {
    let root = process_service_install_root().unwrap_or_else(default_windows_service_root);
    ServiceInstallPaths::for_root(root)
}

pub fn default_windows_service_root() -> PathBuf {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .map(|path| path.join("AppData").join("Local"))
        })
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\Default\AppData\Local"));
    local_app_data.join("Programs").join(WINDOWS_SERVICE_DIR)
}

fn process_service_install_root() -> Option<PathBuf> {
    SERVICE_INSTALL_ROOT_OVERRIDE.get().cloned()
}

fn set_process_service_install_root(root: PathBuf) -> Result<(), ServiceError> {
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

fn windows_service_paths_for_root(
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

fn windows_service_paths_for_install(
    install_root: Option<&Path>,
) -> Result<ServiceInstallPaths, ServiceError> {
    let root = match install_root {
        Some(root) => normalize_install_root(root)?,
        None => default_windows_service_paths().root_dir,
    };
    Ok(ServiceInstallPaths::for_root(root))
}

fn service_paths_from_executable_path(
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

fn service_paths_from_scm_binary_path(
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

fn service_install_root_from_args(args: &[String]) -> Option<PathBuf> {
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

fn install_root_from_config_path(config_path: &Path) -> Option<PathBuf> {
    config_path.parent()?.parent().map(PathBuf::from)
}

fn split_windows_command_line(command_line: &str) -> Vec<String> {
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
                current.extend(std::iter::repeat('\\').take(backslashes / 2));
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
                current.extend(std::iter::repeat('\\').take(backslashes));
                backslashes = 0;
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            ch => {
                current.extend(std::iter::repeat('\\').take(backslashes));
                backslashes = 0;
                current.push(ch);
                in_arg = true;
            }
        }
    }

    current.extend(std::iter::repeat('\\').take(backslashes));
    if in_arg {
        args.push(current);
    }
    args
}

#[cfg(windows)]
fn installed_service_paths_from_scm() -> Option<ServiceInstallPaths> {
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
fn installed_service_paths_from_scm() -> Option<ServiceInstallPaths> {
    None
}

fn normalize_install_root(root: &Path) -> Result<PathBuf, ServiceError> {
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

fn legacy_program_data_service_paths() -> ServiceInstallPaths {
    let root = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("DbgAtlas");
    ServiceInstallPaths::for_root(root)
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

fn installed_client_config_from_paths(
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

fn create_runtime_config_if_missing(
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

fn runtime_config_toml(runtime: &RuntimeConfig) -> String {
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

fn toml_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn prepare_install_layout(paths: &ServiceInstallPaths) -> Result<(), ServiceError> {
    fs::create_dir_all(&paths.root_dir)?;
    fs::create_dir_all(&paths.etc_dir)?;
    fs::create_dir_all(&paths.log_dir)?;
    fs::write(paths.install_marker_path(), "DbgAtlas install root\n")?;
    migrate_legacy_install_file(&paths.legacy_config_path(), &paths.config_path)?;
    migrate_legacy_install_file(&paths.legacy_token_file(), &paths.token_file)?;
    Ok(())
}

fn migrate_legacy_install_file(legacy_path: &Path, target_path: &Path) -> Result<(), ServiceError> {
    if target_path.exists() || !legacy_path.exists() {
        return Ok(());
    }
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(legacy_path, target_path)?;
    Ok(())
}

fn install_payload(
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

fn validate_existing_payload(
    paths: &ServiceInstallPaths,
) -> Result<Vec<ServicePayloadFile>, ServiceError> {
    discover_service_payload(&paths.bin_dir, &paths.bin_dir)
}

fn payload_dir_from_current_exe() -> Result<PathBuf, ServiceError> {
    let current_exe = std::env::current_exe()?;
    current_exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ServiceError::ServiceControl("current executable has no parent".into()))
}

fn copy_missing_install_state_from_previous_root(
    paths: &ServiceInstallPaths,
) -> Result<(), ServiceError> {
    let previous = legacy_program_data_service_paths();
    copy_missing_install_state_from_paths(paths, &previous)
}

fn copy_missing_install_state_from_paths(
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

fn copy_missing_install_file(candidates: &[PathBuf], target: &Path) -> Result<(), ServiceError> {
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

fn cleanup_install_dirs(paths: &ServiceInstallPaths, purge: bool) -> Result<(), ServiceError> {
    if !purge {
        return Ok(());
    }
    validate_purge_install_root(paths)?;
    if paths.root_dir.exists() {
        fs::remove_dir_all(&paths.root_dir)?;
    }
    Ok(())
}

fn validate_purge_install_root(paths: &ServiceInstallPaths) -> Result<(), ServiceError> {
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

fn install_roots_match(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

fn validate_update_timeout(timeout_ms: u64) -> Result<Duration, ServiceError> {
    if timeout_ms == 0 {
        return Err(ServiceError::Rpc(
            "timeout_ms must be greater than 0".to_string(),
        ));
    }
    Ok(Duration::from_millis(timeout_ms))
}

fn prepare_service_update(
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

fn discover_service_config_payload(
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

fn validate_update_runtime_config(path: &Path) -> Result<(), ServiceError> {
    // 先按 TOML 结构拒绝任何 token 形态，再交给 dbgatlas-runtime 做版本和字段校验。
    // 这样 updater 即使被传入错误 payload，也不会把 bearer token 写进 runtime.toml。
    let input = fs::read_to_string(path)?;
    reject_runtime_config_token_keys(&input)?;
    reject_runtime_config_local_debug_tool_keys(&input)?;
    let _runtime = RuntimeConfig::from_toml_str(&input)?;
    Ok(())
}

fn reject_runtime_config_token_keys(input: &str) -> Result<(), ServiceError> {
    let value = toml::from_str::<toml::Value>(input)
        .map_err(|error| dbgatlas_runtime::RuntimeConfigError::ParseToml(error))?;
    let mut path = Vec::new();
    reject_runtime_config_token_keys_in_value(&value, &mut path)
}

fn reject_runtime_config_token_keys_in_value(
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

fn is_runtime_config_token_key(key: &str) -> bool {
    key.chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .collect::<String>()
        .to_ascii_lowercase()
        .contains("token")
}

fn reject_runtime_config_local_debug_tool_keys(input: &str) -> Result<(), ServiceError> {
    let value = toml::from_str::<toml::Value>(input)
        .map_err(|error| dbgatlas_runtime::RuntimeConfigError::ParseToml(error))?;
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

fn copy_update_payload_to_staging(
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

fn copy_update_config_to_staging(
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

fn apply_update_config_payload(
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

fn replace_installed_bin_with_staging(
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

fn replace_installed_bin_once(
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

fn cleanup_update_dirs(paths: &ServiceInstallPaths) -> Result<(), ServiceError> {
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

fn source_is_installed_bin(source_dir: &Path, paths: &ServiceInstallPaths) -> bool {
    let Ok(source) = fs::canonicalize(source_dir) else {
        return false;
    };
    let Ok(destination) = fs::canonicalize(&paths.bin_dir) else {
        return false;
    };
    source == destination
}

#[cfg(not(windows))]
mod windows_service_control {
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
mod windows_service_control {
    use super::*;
    use std::ffi::OsString;
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

    windows_service::define_windows_service!(ffi_service_main, service_main);

    static RUN_OPTIONS: OnceLock<WindowsServiceRunOptions> = OnceLock::new();

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
        ensure_token_file(&paths.token_file)?;
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

    fn service_main(_arguments: Vec<OsString>) {
        append_service_log("entered service_main");
        if let Err(error) = run_service_main() {
            append_service_log(&format!("service_main failed: {error}"));
            eprintln!("DbgAtlas Windows service error: {error:#}");
        }
        append_service_log("leaving service_main");
    }

    fn run_service_main() -> Result<(), ServiceError> {
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

    fn create_or_update_service(
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

    fn previous_install_paths_from_service(
        service: &windows_service::service::Service,
    ) -> Result<ServiceInstallPaths, ServiceError> {
        let config = service.query_config().map_err(map_windows_service_error)?;
        service_paths_from_scm_binary_path(&config.executable_path)
    }

    fn service_info(paths: &ServiceInstallPaths) -> ServiceInfo {
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

    fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, ServiceError> {
        ServiceManager::local_computer(None::<&str>, access).map_err(map_windows_service_error)
    }

    fn open_required(
        manager: &ServiceManager,
        access: ServiceAccess,
    ) -> Result<windows_service::service::Service, ServiceError> {
        open_optional(manager, access)?.ok_or_else(|| {
            ServiceError::ServiceControl(format!(
                "Windows service `{WINDOWS_SERVICE_NAME}` is not installed"
            ))
        })
    }

    fn open_optional(
        manager: &ServiceManager,
        access: ServiceAccess,
    ) -> Result<Option<windows_service::service::Service>, ServiceError> {
        match manager.open_service(WINDOWS_SERVICE_NAME, access) {
            Ok(service) => Ok(Some(service)),
            Err(error) if is_service_not_found(&error) => Ok(None),
            Err(error) => Err(map_windows_service_error(error)),
        }
    }

    fn stop_service(service: &windows_service::service::Service) -> Result<(), ServiceError> {
        stop_service_with_timeout(service, Duration::from_secs(15))
    }

    fn stop_service_with_timeout(
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

    fn start_service_with_timeout(
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

    fn wait_for_state(
        service: &windows_service::service::Service,
        expected: ServiceState,
    ) -> Result<(), ServiceError> {
        wait_for_state_with_timeout(service, expected, Duration::from_secs(15))
    }

    fn wait_for_state_with_timeout(
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

    fn remaining_update_timeout(deadline: Instant) -> Result<Duration, ServiceError> {
        deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                ServiceError::ServiceControl("timed out applying service update".to_string())
            })
    }

    fn update_dir_suffix() -> String {
        format!("{}-{}", Timestamp::now().unix_millis, std::process::id())
    }

    fn set_status(
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

    fn ensure_token_file(path: &Path) -> Result<(), ServiceError> {
        if path.exists() {
            let token = fs::read_to_string(path)?;
            if token.trim().is_empty() {
                return Err(ServiceError::EmptyBearerToken);
            }
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let token = generate_token()?;
        fs::write(path, format!("{token}\n"))?;
        Ok(())
    }

    fn generate_token() -> Result<String, ServiceError> {
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

    fn installed_endpoint(paths: &ServiceInstallPaths) -> Result<Option<SocketAddr>, ServiceError> {
        Ok(installed_client_config_from_paths(paths)?.map(|config| config.bind))
    }

    fn result(
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

    fn state_name(state: ServiceState) -> &'static str {
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

    fn hex_encode(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
        output
    }

    fn is_service_not_found(error: &windows_service::Error) -> bool {
        matches!(
            error,
            windows_service::Error::Winapi(io) if io.raw_os_error() == Some(1060)
        )
    }

    fn is_service_not_active(error: &windows_service::Error) -> bool {
        matches!(
            error,
            windows_service::Error::Winapi(io) if io.raw_os_error() == Some(1062)
        )
    }

    fn map_windows_service_error(error: windows_service::Error) -> ServiceError {
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

    fn append_service_log(message: &str) {
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

    fn date_from_unix_day(day: i64) -> (i32, u32, u32) {
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

    fn days_in_month(year: i32, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if is_leap_year(year) => 29,
            2 => 28,
            _ => 0,
        }
    }

    fn is_leap_year(year: i32) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }
}

struct WorkerTransport {
    file: std::fs::File,
}

struct WorkerPipeServer {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl WorkerTransport {
    fn create_server(pipe_name: &str) -> Result<WorkerPipeServer, ServiceError> {
        create_worker_pipe_server(pipe_name)
    }

    fn request(
        &mut self,
        worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        let request_id = next_worker_request_id();
        let method = request.method_name();
        let session_id = request.session_id().id.as_str().to_string();
        let operation_id = request
            .operation_id()
            .map(|operation| operation.id.as_str().to_string());
        let started = Instant::now();
        append_worker_transport_log(
            "start",
            worker,
            method,
            &request_id,
            &session_id,
            operation_id.as_deref(),
            None,
            None,
            started.elapsed(),
        );
        let envelope = WorkerEnvelope::new(request_id.clone(), request);
        let line = encode_jsonl(&envelope).map_err(|error| {
            append_worker_transport_log(
                "encode_failed",
                worker,
                method,
                &request_id,
                &session_id,
                operation_id.as_deref(),
                None,
                Some(&error.to_string()),
                started.elapsed(),
            );
            ServiceError::from(error)
        })?;
        if let Err(error) = self.file.write_all(line.as_bytes()) {
            append_worker_transport_log(
                "write_failed",
                worker,
                method,
                &request_id,
                &session_id,
                operation_id.as_deref(),
                None,
                Some(&error.to_string()),
                started.elapsed(),
            );
            return Err(error.into());
        }
        if let Err(error) = self.file.flush() {
            append_worker_transport_log(
                "flush_failed",
                worker,
                method,
                &request_id,
                &session_id,
                operation_id.as_deref(),
                None,
                Some(&error.to_string()),
                started.elapsed(),
            );
            return Err(error.into());
        }
        let response_line = read_jsonl_line(&mut self.file).map_err(|error| {
            append_worker_transport_log(
                "read_failed",
                worker,
                method,
                &request_id,
                &session_id,
                operation_id.as_deref(),
                None,
                Some(&error.to_string()),
                started.elapsed(),
            );
            error
        })?;
        let response: WorkerEnvelope<WorkerResponse> =
            decode_jsonl(&response_line).map_err(|error| {
                append_worker_transport_log(
                    "decode_failed",
                    worker,
                    method,
                    &request_id,
                    &session_id,
                    operation_id.as_deref(),
                    None,
                    Some(&error.to_string()),
                    started.elapsed(),
                );
                ServiceError::from(error)
            })?;
        if response.request_id != request_id {
            let message = format!(
                "worker response id mismatch: expected {request_id}, got {}",
                response.request_id
            );
            append_worker_transport_log(
                "response_id_mismatch",
                worker,
                method,
                &request_id,
                &session_id,
                operation_id.as_deref(),
                None,
                Some(&message),
                started.elapsed(),
            );
            return Err(ServiceError::Worker(format!("{message}")));
        }
        append_worker_transport_log(
            "complete",
            worker,
            method,
            &request_id,
            &session_id,
            operation_id.as_deref(),
            Some(worker_response_kind(&response.message)),
            None,
            started.elapsed(),
        );
        Ok(response.message)
    }
}

impl WorkerPipeServer {
    fn connect(self, timeout_ms: u64) -> Result<WorkerTransport, ServiceError> {
        connect_worker_pipe_server(self, timeout_ms)
    }
}

fn next_worker_request_id() -> String {
    let count = WORKER_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("worker-req-{}-{count}", Timestamp::now().unix_millis)
}

fn read_jsonl_line(reader: &mut impl Read) -> Result<String, ServiceError> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let read = reader.read(&mut byte)?;
        if read == 0 {
            return Err(ServiceError::Worker(
                "worker pipe closed before response".to_string(),
            ));
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() > MAX_WORKER_RESPONSE_LINE_BYTES {
            discard_jsonl_line_tail(reader)?;
            return Err(ServiceError::Worker(
                "worker response line is too large".to_string(),
            ));
        }
    }
    String::from_utf8(bytes).map_err(|error| ServiceError::Worker(error.to_string()))
}

fn discard_jsonl_line_tail(reader: &mut impl Read) -> Result<(), ServiceError> {
    let mut byte = [0u8; 1];
    loop {
        let read = reader.read(&mut byte)?;
        if read == 0 || byte[0] == b'\n' {
            return Ok(());
        }
    }
}

#[cfg(windows)]
fn create_worker_pipe_server(pipe_name: &str) -> Result<WorkerPipeServer, ServiceError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let wide_name: Vec<u16> = std::ffi::OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut security = worker_pipe_security()?;
    let handle = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            security.attributes_ptr(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(WorkerPipeServer { handle })
}

#[cfg(windows)]
struct WorkerPipeSecurity {
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    attributes: windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
}

#[cfg(windows)]
impl WorkerPipeSecurity {
    fn attributes_ptr(&mut self) -> *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        &mut self.attributes
    }
}

#[cfg(windows)]
impl Drop for WorkerPipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(self.descriptor as _);
            }
        }
    }
}

#[cfg(windows)]
fn worker_pipe_security() -> Result<WorkerPipeSecurity, ServiceError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    // LocalSystem service creates the server end; active interactive user workers need client access.
    let sddl = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)";
    let wide_sddl: Vec<u16> = std::ffi::OsStr::new(sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(WorkerPipeSecurity {
        descriptor,
        attributes: SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        },
    })
}

#[cfg(not(windows))]
fn create_worker_pipe_server(_pipe_name: &str) -> Result<WorkerPipeServer, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}

#[cfg(windows)]
fn connect_worker_pipe_server(
    server: WorkerPipeServer,
    timeout_ms: u64,
) -> Result<WorkerTransport, ServiceError> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GetLastError, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::ConnectNamedPipe;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event;
    let connected = unsafe { ConnectNamedPipe(server.handle, &mut overlapped) };
    if connected == 0 {
        let error = unsafe { GetLastError() };
        match error {
            ERROR_PIPE_CONNECTED => {}
            ERROR_IO_PENDING => {
                let wait =
                    unsafe { WaitForSingleObject(event, timeout_ms.min(u32::MAX as u64) as u32) };
                if wait == WAIT_TIMEOUT {
                    unsafe {
                        CancelIoEx(server.handle, &overlapped);
                        CloseHandle(event);
                    }
                    return Err(ServiceError::Worker(format!(
                        "timed out waiting for worker pipe connection after {timeout_ms} ms"
                    )));
                }
                if wait != WAIT_OBJECT_0 {
                    unsafe {
                        CancelIoEx(server.handle, &overlapped);
                        CloseHandle(event);
                    }
                    return Err(std::io::Error::last_os_error().into());
                }
                let mut transferred = 0;
                let ok =
                    unsafe { GetOverlappedResult(server.handle, &overlapped, &mut transferred, 0) };
                if ok == 0 {
                    unsafe {
                        CloseHandle(event);
                    }
                    return Err(std::io::Error::last_os_error().into());
                }
            }
            _ => {
                unsafe {
                    CloseHandle(event);
                }
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    unsafe {
        CloseHandle(event);
    }
    let handle = server.handle;
    std::mem::forget(server);
    let file = unsafe { std::fs::File::from_raw_handle(handle as RawHandle) };
    Ok(WorkerTransport { file })
}

#[cfg(not(windows))]
fn connect_worker_pipe_server(
    _server: WorkerPipeServer,
    _timeout_ms: u64,
) -> Result<WorkerTransport, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}

#[cfg(windows)]
impl Drop for WorkerPipeServer {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};

        if self.handle != INVALID_HANDLE_VALUE && !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ServiceOperation {
    pub operation_id: OperationRef,
    pub capability: String,
    pub session_id: Option<SessionRef>,
    pub status: ServiceOperationStatus,
    pub summary: String,
    pub artifacts: Vec<ArtifactRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<ArtifactRef>,
    pub events: Vec<ServiceEvent>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl ServiceOperation {
    fn running(
        operation_id: OperationRef,
        capability: impl Into<String>,
        session_id: Option<SessionRef>,
        summary: impl Into<String>,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            operation_id,
            capability: capability.into(),
            session_id,
            status: ServiceOperationStatus::Running,
            summary: summary.into(),
            artifacts: Vec::new(),
            raw_output: None,
            events: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn success(
        operation_id: OperationRef,
        capability: impl Into<String>,
        session_id: Option<SessionRef>,
        summary: impl Into<String>,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            operation_id,
            capability: capability.into(),
            session_id,
            status: ServiceOperationStatus::Success,
            summary: summary.into(),
            artifacts: Vec::new(),
            raw_output: None,
            events: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn failed(
        operation_id: OperationRef,
        capability: impl Into<String>,
        session_id: Option<SessionRef>,
        summary: impl Into<String>,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            operation_id,
            capability: capability.into(),
            session_id,
            status: ServiceOperationStatus::Failed,
            summary: summary.into(),
            artifacts: Vec::new(),
            raw_output: None,
            events: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceOperationStatus {
    Running,
    Success,
    Failed,
    Canceled,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServiceEvent {
    pub timestamp: Timestamp,
    pub kind: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    fn result(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

struct ToolCallOutput {
    value: Value,
    is_error: bool,
    operation_id: Option<String>,
    error_code: Option<i64>,
    error_message: Option<String>,
}

impl ToolCallOutput {
    fn success(value: Value) -> Self {
        let operation_id = extract_ref_id_field(&value, "operation_id");
        Self {
            value,
            is_error: false,
            operation_id,
            error_code: None,
            error_message: None,
        }
    }

    fn error(value: Value) -> Self {
        let operation_id = extract_ref_id_field(&value, "operation_id");
        let error_code = value
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64);
        let error_message = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Self {
            value,
            is_error: true,
            operation_id,
            error_code,
            error_message,
        }
    }

    fn set_operation_id(&mut self, operation_id: String) {
        self.operation_id = Some(operation_id.clone());
        if self.is_error {
            if let Value::Object(object) = &mut self.value {
                object
                    .entry("operation_id")
                    .or_insert_with(|| json!({ "id": operation_id }));
            }
        }
    }
}

fn mcp_service_response_result(response: JsonRpcResponse) -> ToolCallOutput {
    if let Some(error) = response.error {
        return ToolCallOutput::error(json!({
            "error": {
                "code": error.code,
                "message": error.message,
            }
        }));
    }
    ToolCallOutput::success(response.result.unwrap_or_else(|| json!(null)))
}

#[derive(Default)]
struct DiagnosticContext {
    remote_addr: Option<String>,
    path: Option<String>,
    rpc_method: Option<String>,
    rpc_id: Option<String>,
    mcp_tool: Option<String>,
    session_id: Option<String>,
    operation_id: Option<String>,
}

impl DiagnosticContext {
    fn from_rpc(request: &JsonRpcRequest) -> Self {
        let mut context = Self {
            rpc_method: Some(request.method.clone()),
            rpc_id: request.id.as_ref().map(rpc_id_for_log),
            ..Default::default()
        };
        if request.method == "tools/call" {
            if let Some(params) = &request.params {
                context.mcp_tool = params
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if let Some(arguments) = params.get("arguments") {
                    context.session_id = extract_ref_id_field(arguments, "session_id");
                    context.operation_id = extract_ref_id_field(arguments, "operation_id");
                }
            }
        } else if let Some(params) = &request.params {
            context.session_id = extract_ref_id_field(params, "session_id");
            context.operation_id = extract_ref_id_field(params, "operation_id");
        }
        context
    }

    fn log_fields(&self) -> String {
        format!(
            "remote={} path={} rpc_method={} rpc_id={} mcp_tool={} session_id={} operation_id={}",
            log_value(self.remote_addr.as_deref()),
            log_value(self.path.as_deref()),
            log_value(self.rpc_method.as_deref()),
            log_value(self.rpc_id.as_deref()),
            log_value(self.mcp_tool.as_deref()),
            log_value(self.session_id.as_deref()),
            log_value(self.operation_id.as_deref())
        )
    }
}

fn append_mcp_tool_error_log(name: &str, arguments: &Value, result: &ToolCallOutput) {
    let session_id = extract_ref_id_field(arguments, "session_id");
    let operation_id = result
        .operation_id
        .clone()
        .or_else(|| extract_ref_id_field(arguments, "operation_id"));
    let error_code = result
        .error_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let error_message = result.error_message.as_deref().unwrap_or("unknown");
    append_service_diagnostic_log(&format!(
        "mcp_tool_error tool={} session_id={} operation_id={} error_code={} error={}",
        sanitize_log_value(name),
        log_value(session_id.as_deref()),
        log_value(operation_id.as_deref()),
        sanitize_log_value(&error_code),
        sanitize_log_value(error_message)
    ));
}

fn append_http_diagnostic_log(stage: &str, context: &DiagnosticContext, error: &str) {
    append_service_diagnostic_log(&format!(
        "http_request_error stage={} {} error={}",
        sanitize_log_value(stage),
        context.log_fields(),
        sanitize_log_value(error)
    ));
}

fn append_http_success_log(status: u16, duration: Duration, context: &DiagnosticContext) {
    append_service_diagnostic_log(&format!(
        "http_request_complete status={} duration_ms={} {}",
        status,
        duration.as_millis(),
        context.log_fields()
    ));
}

fn append_worker_transport_log(
    event: &str,
    worker: &WorkerHandle,
    method: &str,
    request_id: &str,
    session_id: &str,
    operation_id: Option<&str>,
    response: Option<&str>,
    error: Option<&str>,
    duration: Duration,
) {
    append_service_diagnostic_log(&format!(
        "worker_transport event={} worker_id={} identity={} method={} request_id={} session_id={} operation_id={} response={} duration_ms={} error={}",
        sanitize_log_value(event),
        sanitize_log_value(worker.worker_id.as_str()),
        sanitize_log_value(&format!("{:?}", worker.identity)),
        sanitize_log_value(method),
        sanitize_log_value(request_id),
        sanitize_log_value(session_id),
        log_value(operation_id),
        log_value(response),
        duration.as_millis(),
        log_value(error),
    ));
}

fn worker_response_kind(response: &WorkerResponse) -> &'static str {
    match response {
        WorkerResponse::Ok { .. } => "ok",
        WorkerResponse::DebugCommand { .. } => "debug_command",
        WorkerResponse::DebugMemory { .. } => "debug_memory",
        WorkerResponse::ReverseSessionOpened { .. } => "reverse_session_opened",
        WorkerResponse::ReverseFunctionLookup { .. } => "reverse_function_lookup",
        WorkerResponse::ReverseCoreFunction { .. } => "reverse_core_function",
        WorkerResponse::Failed { .. } => "failed",
    }
}

fn append_operation_diagnostic_log(
    capability: &str,
    session_id: &SessionRef,
    operation_id: &OperationRef,
    status: &OperationStatus,
    artifact_count: usize,
    raw_output: Option<&ArtifactRef>,
    error: Option<&str>,
) {
    // 这里故意只记录 refs 和摘要级错误，不记录命令正文、token 或完整参数。
    // 需要复现时再通过 workspace JSONL 和 artifact refs 进入事实层。
    append_service_diagnostic_log(&format!(
        "operation_recorded capability={} session_id={} operation_id={} status={} artifact_count={} raw_output_ref={} error={}",
        sanitize_log_value(capability),
        sanitize_log_value(session_id.id.as_str()),
        sanitize_log_value(operation_id.id.as_str()),
        sanitize_log_value(&format!("{status:?}")),
        artifact_count,
        log_value(raw_output.map(|artifact| artifact.id.as_str())),
        log_value(error)
    ));
}

fn append_service_diagnostic_log(message: &str) {
    let timestamp = Timestamp::now().unix_millis;
    let day = (timestamp / 86_400_000) as i64;
    let log_path = default_windows_service_paths()
        .log_dir
        .join(format!("service-{}.log", log_utc_date_from_unix_day(day)));
    let _ = fs::create_dir_all(log_path.parent().unwrap_or_else(|| Path::new(".")));
    let line = format!("{timestamp} {}\n", sanitize_log_value(message));
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
}

fn log_value(value: Option<&str>) -> String {
    value
        .map(sanitize_log_value)
        .unwrap_or_else(|| "unknown".to_string())
}

fn rpc_id_for_log(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => "null".to_string(),
        _ => "complex".to_string(),
    }
}

fn sanitize_log_value(value: &str) -> String {
    let mut sanitized = String::new();
    for ch in value.chars() {
        if sanitized.len() >= 512 {
            sanitized.push_str("...");
            break;
        }
        if ch.is_control() || ch.is_whitespace() {
            sanitized.push(' ');
        } else {
            sanitized.push(ch);
        }
    }
    if sanitized.is_empty() {
        "empty".to_string()
    } else {
        sanitized
    }
}

fn extract_ref_id_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(extract_ref_id_value)
}

fn extract_ref_id_value(value: &Value) -> Option<String> {
    value.as_str().map(ToOwned::to_owned).or_else(|| {
        value
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn log_utc_date_from_unix_day(day: i64) -> String {
    let (year, month, day) = log_date_from_unix_day(day);
    format!("{year:04}-{month:02}-{day:02}")
}

fn log_date_from_unix_day(day: i64) -> (i32, u32, u32) {
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

fn mcp_tool_descriptors(capabilities: ServiceCapabilities) -> Vec<Value> {
    let mut tools = vec![
        mcp_tool(
            "service.health",
            "Return DbgAtlas service health.",
            json!({}),
        ),
        mcp_tool(
            "service.info",
            "Return DbgAtlas service information.",
            json!({}),
        ),
        mcp_tool(
            "service.update",
            "Update the installed DbgAtlas service from a built payload directory.",
            json!({
                "type": "object",
                "properties": {
                    "source_dir": { "type": "string" },
                    "restart": { "type": "boolean", "default": true },
                    "timeout_ms": { "type": "integer", "default": DEFAULT_SERVICE_UPDATE_TIMEOUT_MS }
                },
                "required": ["source_dir"]
            }),
        ),
        mcp_tool(
            "debug.session.create",
            "Create a debug session from a file, attach, or launch target. Use kind=file with a .run path for TTD replay.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "target": debug_target_schema(),
                    "startup_timeout_ms": { "type": "integer" },
                    "worker_identity": {
                        "type": "string",
                        "enum": ["default", "active_interactive_user"],
                        "default": "default",
                        "description": "Use active_interactive_user for user-session TTD replay or live launch when the installed service default LocalSystem identity cannot access the target."
                    }
                },
                "required": ["project_root", "target"]
            }),
        ),
        mcp_tool(
            "recording.ttd",
            "Record a Time Travel Debugging trace with TTD.exe and register the .run/.idx artifacts. To replay an existing .run trace, use debug.session.create with target kind=file.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "target": ttd_recording_target_schema(),
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Maximum recording duration in milliseconds before DbgAtlas asks TTD.exe to stop and keeps any completed trace artifacts."
                    },
                    "worker_identity": {
                        "type": "string",
                        "enum": ["default", "active_interactive_user"],
                        "default": "default",
                        "description": "Run TTD.exe as the service process by default. Use active_interactive_user when an installed service must attach to a process in the current interactive user session."
                    },
                    "options": ttd_recording_options_schema()
                },
                "required": ["project_root", "target", "timeout_ms"]
            }),
        ),
        mcp_tool(
            "debug.eval",
            "Execute one raw DbgEng IDebugControl::Execute command string in an existing session. This does not emulate WinDbg command-window multiline input; use debug.eval_steps for ordered multi-step commands.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "command": {
                        "type": "string",
                        "description": "Raw DbgEng command string sent as a single IDebugControl::Execute call. Newlines are not treated as separate WinDbg command-window submissions; use debug.eval_steps when commands such as .exepath/.sympath must be isolated from following commands."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Optional command timeout in milliseconds. On timeout DbgAtlas kills the debug worker and the session must be recreated."
                    }
                },
                "required": ["session_id", "command"]
            }),
        ),
        mcp_tool(
            "debug.eval_steps",
            "Execute raw DbgEng commands one step at a time in an existing session, with each array item sent as a separate IDebugControl::Execute call.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "commands": {
                        "type": "array",
                        "description": "Ordered command steps. Each item is executed separately, so line-consuming commands such as .exepath/.sympath do not consume later steps.",
                        "items": { "type": "string" },
                        "minItems": 1
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Optional timeout per step in milliseconds. On timeout DbgAtlas kills the debug worker and the session must be recreated."
                    },
                    "continue_on_error": {
                        "type": "boolean",
                        "default": false
                    }
                },
                "required": ["session_id", "commands"]
            }),
        ),
        mcp_tool(
            "debug.modules",
            "List modules for a debug session. In TTD replay, modules that were loaded and unloaded outside the current position may require targeted TTD calls or memory evidence.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.threads",
            "List threads for a debug session.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.stack",
            "Get stack for a debug session.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.add_symbols",
            "Append a symbol path to a debug session using .sympath+. Set reload=true to run .reload after updating the path.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "symbol_path": {
                        "type": "string",
                        "description": "Symbol path segment to append with .sympath+; this does not replace the existing symbol path."
                    },
                    "reload": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, execute .reload after appending the symbol path."
                    }
                },
                "required": ["session_id", "symbol_path"]
            }),
        ),
        mcp_tool(
            "debug.read_memory",
            "Read virtual memory to an artifact.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "address": {},
                    "length": { "type": "integer" }
                },
                "required": ["session_id", "address", "length"]
            }),
        ),
        mcp_tool(
            "reverse.session.open",
            "Open an IDA reverse session for an IDB/database path. This is not a read-only clone; opening an IDB already held by another IDA instance can fail with lock/availability diagnostics.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "database_path": {
                        "type": "string",
                        "description": "Path to the IDA database or input file to open in the session."
                    },
                    "ida_install_dir": {
                        "type": "string",
                        "description": "Optional IDA installation directory override."
                    }
                },
                "required": ["project_root", "database_path"]
            }),
        ),
        mcp_tool(
            "reverse.lookup_function",
            "Map a runtime address to an IDA function and record the lookup.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "runtime_address": {},
                    "runtime_module_base": {},
                    "ida_image_base": {}
                },
                "required": [
                    "session_id",
                    "runtime_address",
                    "runtime_module_base",
                    "ida_image_base"
                ]
            }),
        ),
        mcp_tool(
            "reverse.lookup_funcs",
            "Lookup IDA functions by address or name.",
            mcp_reverse_core_schema_required(
                json!({
                "queries": {},
                "runtime_module_base": {},
                "ida_image_base": {}
                }),
                &["queries"],
            ),
        ),
        mcp_tool(
            "reverse.int_convert",
            "Convert decimal, hex, bytes, ASCII, and binary integer representations.",
            mcp_reverse_core_schema_required(json!({ "inputs": {} }), &["inputs"]),
        ),
        mcp_tool(
            "reverse.list_funcs",
            "List IDA functions with pagination and optional filtering.",
            mcp_reverse_core_schema(json!({
                "offset": { "type": "integer" },
                "count": { "type": "integer" },
                "filter": { "type": "string" }
            })),
        ),
        mcp_tool(
            "reverse.list_globals",
            "List IDA global variables with pagination and optional filtering.",
            mcp_reverse_core_schema(json!({
                "offset": { "type": "integer" },
                "count": { "type": "integer" },
                "filter": { "type": "string" }
            })),
        ),
        mcp_tool(
            "reverse.list_imports",
            "List imported symbols and module names with pagination.",
            mcp_reverse_core_schema(json!({
                "offset": { "type": "integer" },
                "count": { "type": "integer" },
                "filter": { "type": "string" }
            })),
        ),
        mcp_tool(
            "reverse.list_strings",
            "List IDA strings with pagination and optional substring filtering.",
            mcp_reverse_core_schema(json!({
                "offset": { "type": "integer" },
                "count": { "type": "integer" },
                "filter": { "type": "string" }
            })),
        ),
        mcp_tool(
            "reverse.get_string",
            "Read an IDA string at an address.",
            mcp_reverse_core_schema_required(
                json!({
                    "addr": {},
                    "length": { "type": "integer" },
                    "type": { "type": "integer" }
                }),
                &["addr"],
            ),
        ),
        mcp_tool(
            "reverse.get_bytes",
            "Read IDB bytes at an address.",
            mcp_reverse_core_schema_required(
                json!({
                    "addr": {},
                    "length": { "type": "integer" }
                }),
                &["addr", "length"],
            ),
        ),
        mcp_tool(
            "reverse.get_int",
            "Read an integer from IDB bytes at an address.",
            mcp_reverse_core_schema_required(
                json!({
                    "addr": {},
                    "size": { "type": "integer" },
                    "endian": {
                        "type": "string",
                        "enum": ["little", "big"],
                        "default": "little"
                    }
                }),
                &["addr"],
            ),
        ),
        mcp_tool(
            "reverse.decompile",
            "Decompile the function containing an IDA address.",
            mcp_reverse_core_schema_required(json!({ "addr": {} }), &["addr"]),
        ),
        mcp_tool(
            "reverse.disasm",
            "Disassemble the function containing an IDA address.",
            mcp_reverse_core_schema_required(json!({ "addr": {} }), &["addr"]),
        ),
        mcp_tool(
            "reverse.xrefs_to",
            "Find cross-references to one or more IDA addresses.",
            mcp_reverse_core_schema_required(json!({ "addrs": {} }), &["addrs"]),
        ),
        mcp_tool(
            "reverse.xrefs_to_field",
            "Find cross-references to struct fields.",
            mcp_reverse_core_schema_required(json!({ "queries": {} }), &["queries"]),
        ),
        mcp_tool(
            "reverse.callees",
            "List callees for one or more IDA functions.",
            mcp_reverse_core_schema_required(json!({ "addrs": {} }), &["addrs"]),
        ),
        mcp_tool(
            "reverse.rename",
            "Rename IDA functions or globals in the current session. This mutates the IDB; call reverse.idb_save to persist changes when needed.",
            mcp_reverse_core_schema_required(
                json!({ "items": reverse_rename_items_schema() }),
                &["items"],
            ),
        ),
        mcp_tool(
            "reverse.set_comments",
            "Set IDA comments in the current session. This mutates the IDB; call reverse.idb_save to persist changes when needed.",
            mcp_reverse_core_schema_required(
                json!({ "items": reverse_set_comments_items_schema() }),
                &["items"],
            ),
        ),
        mcp_tool(
            "reverse.set_type",
            "Apply C types to IDA functions, globals, or addresses in the current session. This mutates the IDB; call reverse.idb_save to persist changes when needed.",
            mcp_reverse_core_schema_required(
                json!({ "items": reverse_set_type_items_schema() }),
                &["items"],
            ),
        ),
        mcp_tool(
            "reverse.declare_type",
            "Declare C types in the IDA local type library. This mutates the IDB; call reverse.idb_save to persist changes when needed.",
            mcp_reverse_core_schema_required(json!({ "decls": {} }), &["decls"]),
        ),
        mcp_tool(
            "reverse.inspect_item",
            "Inspect IDA item boundaries, head address, current name, and data/code state without mutating the IDB.",
            mcp_reverse_core_schema_required(json!({ "queries": {} }), &["queries"]),
        ),
        mcp_tool(
            "reverse.force_recompile",
            "Invalidate Hex-Rays cached decompilation for functions or all functions in the current session.",
            mcp_reverse_core_schema(json!({ "addrs": {} })),
        ),
        mcp_tool(
            "reverse.idb_save",
            "Save the current IDA database. When path is omitted, save in place.",
            mcp_reverse_core_schema(json!({
                "path": {
                    "type": "string",
                    "description": "Optional output path. Omit to save the currently opened database in place."
                }
            })),
        ),
        mcp_tool(
            "reverse.find_bytes",
            "Find byte patterns in the IDA database.",
            mcp_reverse_core_schema_required(
                json!({
                    "patterns": {},
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                }),
                &["patterns"],
            ),
        ),
        mcp_tool(
            "reverse.search_text",
            "Search IDA strings, names, disassembly, and comments by substring.",
            mcp_reverse_core_schema_required(
                json!({
                    "query": { "type": "string" },
                    "scope": {
                        "type": "string",
                        "enum": ["strings", "names", "disasm", "comments", "all"]
                    },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                }),
                &["query"],
            ),
        ),
        mcp_tool(
            "reverse.query_xrefs",
            "Query cross-references to or from an address or name.",
            mcp_reverse_core_schema_required(
                json!({
                    "target": {},
                    "direction": { "type": "string", "enum": ["to", "from"] },
                    "xref_type": { "type": "string", "enum": ["code", "data", "all"] },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                }),
                &["target"],
            ),
        ),
        mcp_tool(
            "reverse.query_funcs",
            "Query IDA functions with richer filtering and sorting.",
            mcp_reverse_core_schema(json!({
                "filter": { "type": "string" },
                "name_regex": { "type": "string" },
                "min_size": { "type": "integer" },
                "max_size": { "type": "integer" },
                "has_type": { "type": "boolean" },
                "sort_by": { "type": "string", "enum": ["addr", "name", "size"] },
                "descending": { "type": "boolean" },
                "offset": { "type": "integer" },
                "count": { "type": "integer" }
            })),
        ),
        mcp_tool(
            "reverse.query_entities",
            "Query IDB entities with filtering and pagination.",
            mcp_reverse_core_schema_required(
                json!({
                    "kind": {
                        "type": "string",
                        "enum": ["functions", "globals", "imports", "strings", "names"]
                    },
                    "filter": { "type": "string" },
                    "fields": {},
                    "offset": { "type": "integer" },
                    "count": { "type": "integer" }
                }),
                &["kind"],
            ),
        ),
        mcp_tool(
            "reverse.session.close",
            "Close an IDA reverse session.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema()
                },
                "required": ["session_id"]
            }),
        ),
        mcp_tool(
            "debug.session.close",
            "Cooperatively close a debug session. If the worker is stuck or the pipe is broken, use debug.session.kill.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.session.kill",
            "Forcefully terminate a debug session worker and mark the session closed.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "operation.get",
            "Return an operation status and artifact refs.",
            mcp_operation_schema(),
        ),
        mcp_tool(
            "operation.cancel",
            "Cancel a running operation.",
            mcp_operation_schema(),
        ),
        mcp_tool(
            "operation.stream",
            "Return operation events.",
            mcp_operation_schema(),
        ),
        mcp_tool(
            "workspace.facts",
            "Read workspace facts: artifact registry, operations, and command audit.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        ),
    ];
    if capabilities.ida_py_eval {
        let py_eval = mcp_tool(
            "reverse.py_eval",
            "Execute Python code in the IDA context.",
            mcp_reverse_core_schema_required(json!({ "code": { "type": "string" } }), &["code"]),
        );
        let insert_at = tools
            .iter()
            .position(|tool| tool["name"] == "reverse.find_bytes")
            .unwrap_or(tools.len());
        tools.insert(insert_at, py_eval);
    }
    tools
}

fn mcp_tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn debug_target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "file" },
                    "path": {
                        "type": "string",
                        "description": "Path to a dump or TTD .run trace. TTD replay uses kind=file, not kind=ttd."
                    }
                },
                "required": ["kind", "path"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "attach" },
                    "pid": { "type": "integer", "minimum": 1 }
                },
                "required": ["kind", "pid"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "launch" },
                    "executable": { "type": "string" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "default": []
                    }
                },
                "required": ["kind", "executable"]
            }
        ]
    })
}

fn ttd_recording_target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "launch" },
                    "executable": { "type": "string" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "default": []
                    }
                },
                "required": ["kind", "executable"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "attach" },
                    "pid": { "type": "integer", "minimum": 1 }
                },
                "required": ["kind", "pid"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "monitor" },
                    "program": { "type": "string" },
                    "cmd_line_filter": { "type": "string" }
                },
                "required": ["kind", "program"]
            }
        ]
    })
}

fn ttd_recording_options_schema() -> Value {
    json!({
        "type": "object",
        "description": "TTD.exe recording options. Defaults: no_ui=true, max_file_mb=2048, accept_eula=false, children=false, ring=false, modules=[], record_mode=automatic, replay_cpu_support=default.",
        "properties": {
            "children": {
                "type": "boolean",
                "default": false,
                "description": "Pass -children so child processes are recorded too."
            },
            "no_ui": {
                "type": "boolean",
                "default": true,
                "description": "Pass -noUI to suppress the TTD recorder UI."
            },
            "accept_eula": {
                "type": "boolean",
                "default": false,
                "description": "Pass -accepteula to TTD.exe when the caller has already accepted the TTD license terms."
            },
            "ring": {
                "type": "boolean",
                "default": false,
                "description": "Pass -ring to record in ring-buffer mode."
            },
            "max_file_mb": {
                "type": "integer",
                "default": 2048,
                "minimum": 1,
                "description": "Pass -maxFile. Ring mode is limited to 32768 MB; non-ring mode is limited to 1048576 MB."
            },
            "modules": {
                "type": "array",
                "items": { "type": "string" },
                "default": [],
                "description": "Module filters passed as repeated -module arguments. Non-monitor targets allow at most 64 modules."
            },
            "record_mode": {
                "type": "string",
                "enum": ["automatic", "manual"],
                "default": "automatic",
                "description": "TTD -recordmode value."
            },
            "replay_cpu_support": {
                "type": "string",
                "enum": [
                    "default",
                    "most_conservative",
                    "most_aggressive",
                    "intel_avx_required",
                    "intel_avx2_required"
                ],
                "default": "default",
                "description": "TTD -replayCpuSupport value."
            }
        }
    })
}

fn reverse_rename_items_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "description": "Each item must include addr or name to identify the target.",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["function", "global"],
                    "description": "Use function to normalize addr/name to the containing function start; use global for the exact named/addressed item."
                },
                "addr": {
                    "description": "IDA address to rename. Provide addr or name."
                },
                "name": {
                    "type": "string",
                    "description": "Existing IDA name to rename. Provide addr or name."
                },
                "new_name": { "type": "string" }
            },
            "required": ["kind", "new_name"]
        }
    })
}

fn reverse_set_comments_items_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "description": "Each item must include addr or name to identify the comment location.",
            "properties": {
                "addr": {
                    "description": "IDA address to comment. Provide addr or name."
                },
                "name": {
                    "type": "string",
                    "description": "Existing IDA name whose address should receive the comment. Provide addr or name."
                },
                "text": { "type": "string" },
                "repeatable": { "type": "boolean", "default": false }
            },
            "required": ["text"]
        }
    })
}

fn reverse_set_type_items_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "description": "Each item must include addr or name to identify where the type is applied.",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["function", "global", "addr"],
                    "description": "Use function to apply a function declaration at the function start; global or addr applies at the resolved address."
                },
                "addr": {
                    "description": "IDA address to type. Provide addr or name."
                },
                "name": {
                    "type": "string",
                    "description": "Existing IDA name to type. Provide addr or name."
                },
                "type": { "type": "string" }
            },
            "required": ["kind", "type"]
        }
    })
}

fn ref_id_schema(label: &str, example: &str) -> Value {
    json!({
        "oneOf": [
            {
                "type": "string",
                "description": format!("{label} id string, for example \"{example}\".")
            },
            {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"],
                "additionalProperties": true,
                "description": format!("{label} ref object, for example {{\"id\":\"{example}\"}}.")
            }
        ],
        "description": format!(
            "Accepts either a raw {label} id string such as \"{example}\" or a ref object with an id field."
        )
    })
}

fn session_ref_schema() -> Value {
    ref_id_schema("session", "session-123")
}

fn operation_ref_schema() -> Value {
    ref_id_schema("operation", "op-123")
}

fn mcp_session_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session_id": session_ref_schema()
        },
        "required": ["session_id"]
    })
}

fn mcp_reverse_core_schema(extra_properties: Value) -> Value {
    mcp_reverse_core_schema_required(extra_properties, &[])
}

fn mcp_reverse_core_schema_required(extra_properties: Value, extra_required: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("session_id".to_string(), session_ref_schema());
    if let Value::Object(extra) = extra_properties {
        for (key, value) in extra {
            properties.insert(key, value);
        }
    }
    let mut required = vec![json!("session_id")];
    required.extend(extra_required.iter().map(|field| json!(field)));
    json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

fn mcp_operation_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation_id": operation_ref_schema()
        },
        "required": ["operation_id"]
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize)]
struct DebugSessionCreateParams {
    project_root: PathBuf,
    target: DebugTarget,
    #[serde(default)]
    startup_timeout_ms: Option<u64>,
    #[serde(default)]
    worker_identity: DebugWorkerIdentity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DebugWorkerIdentity {
    #[default]
    Default,
    ActiveInteractiveUser,
}

impl DebugWorkerIdentity {
    fn worker_create_identity(self) -> Option<WorkerIdentity> {
        match self {
            Self::Default => None,
            Self::ActiveInteractiveUser => Some(WorkerIdentity::ActiveInteractiveUser),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct SessionParams {
    session_id: SessionRef,
}

#[derive(Clone, Debug, Deserialize)]
struct RecordingStartParams {
    project_root: PathBuf,
    target: RecordingTarget,
    #[serde(default = "dbgatlas_recording::default_presets")]
    presets: Vec<RecordingPreset>,
}

#[derive(Clone, Debug, Deserialize)]
struct RecordingTtdParams {
    project_root: PathBuf,
    target: TtdTarget,
    timeout_ms: u64,
    #[serde(default)]
    options: TtdRecordingOptions,
    #[serde(default)]
    worker_identity: TtdRecorderIdentity,
}

#[derive(Clone, Debug, Deserialize)]
struct RecordingParams {
    recording_id: RecordingRef,
}

#[derive(Clone, Debug, Deserialize)]
struct DebugEvalParams {
    session_id: SessionRef,
    command: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct DebugEvalStepsParams {
    session_id: SessionRef,
    commands: Vec<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    continue_on_error: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct DebugAddSymbolsParams {
    session_id: SessionRef,
    symbol_path: String,
    #[serde(default)]
    reload: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct DebugReadMemoryParams {
    session_id: SessionRef,
    address: Value,
    length: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct ReverseSessionOpenParams {
    project_root: PathBuf,
    database_path: PathBuf,
    #[serde(default)]
    ida_install_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReverseLookupFunctionParams {
    session_id: SessionRef,
    runtime_address: Value,
    runtime_module_base: Value,
    ida_image_base: Value,
}

#[derive(Clone, Debug, Deserialize)]
struct ReverseCoreFunctionParams {
    session_id: SessionRef,
    #[serde(flatten)]
    arguments: HashMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReverseSessionCloseParams {
    session_id: SessionRef,
}

#[derive(Clone, Debug, Deserialize)]
struct ServiceUpdateParams {
    source_dir: PathBuf,
    #[serde(default = "default_service_update_restart")]
    restart: bool,
    #[serde(default = "default_service_update_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct OperationGetParams {
    operation_id: OperationRef,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceFactsParams {
    path: PathBuf,
}

fn default_service_update_restart() -> bool {
    true
}

fn default_service_update_timeout_ms() -> u64 {
    DEFAULT_SERVICE_UPDATE_TIMEOUT_MS
}

#[derive(Clone, Copy)]
enum SessionFinishMode {
    Close,
    Kill,
}

#[derive(Clone, Copy)]
enum RecordingFinishMode {
    Stop,
    Cancel,
    Kill,
}

pub fn run_http_service(config: ServiceConfig, host: ServiceHost) -> Result<(), ServiceError> {
    run_http_service_until(config, host, ServiceShutdown::new())
}

pub fn run_http_service_until(
    config: ServiceConfig,
    host: ServiceHost,
    shutdown: ServiceShutdown,
) -> Result<(), ServiceError> {
    validate_config(&config)?;
    let listener = TcpListener::bind(config.bind)?;
    listener.set_nonblocking(true)?;
    append_service_diagnostic_log(&format!(
        "http_service_start bind={}",
        sanitize_log_value(&config.bind.to_string())
    ));
    while !shutdown.is_stopping() {
        let (mut stream, peer_addr) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let config = config.clone();
        let host = host.clone();
        std::thread::spawn(move || {
            let response_result = handle_http_stream(&mut stream, &config, &host, Some(peer_addr));
            let (response, write_stage) = match response_result {
                Ok(response) => (response, "write_response"),
                Err(error) => match http_json_response(
                    http_status_for(&error),
                    &JsonRpcResponse::error(None, rpc_error_for(error)),
                ) {
                    Ok(response) => (response, "write_error_response"),
                    Err(error) => {
                        if let Err(write_error) = stream.write_all(error.to_string().as_bytes()) {
                            append_http_diagnostic_log(
                                "write_error_response",
                                &DiagnosticContext {
                                    remote_addr: Some(peer_addr.to_string()),
                                    ..Default::default()
                                },
                                &write_error.to_string(),
                            );
                        }
                        return;
                    }
                },
            };
            if let Err(error) = stream.write_all(response.as_bytes()) {
                append_http_diagnostic_log(
                    write_stage,
                    &DiagnosticContext {
                        remote_addr: Some(peer_addr.to_string()),
                        ..Default::default()
                    },
                    &error.to_string(),
                );
            }
        });
    }
    append_service_diagnostic_log(&format!(
        "http_service_stop bind={}",
        sanitize_log_value(&config.bind.to_string())
    ));
    Ok(())
}

pub fn invoke_http_json_rpc(
    endpoint: SocketAddr,
    bearer_token: &str,
    request: &JsonRpcRequest,
) -> Result<JsonRpcResponse, ServiceError> {
    let body = serde_json::to_string(request)?;
    let mut stream = TcpStream::connect(endpoint)?;
    write!(
        stream,
        "POST /rpc HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        endpoint,
        bearer_token,
        body.len(),
        body
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (_, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| ServiceError::InvalidHttpRequest("missing response body".to_string()))?;
    Ok(serde_json::from_str(body)?)
}

fn handle_http_stream(
    stream: &mut TcpStream,
    config: &ServiceConfig,
    host: &ServiceHost,
    peer_addr: Option<SocketAddr>,
) -> Result<String, ServiceError> {
    let started = Instant::now();
    let request = match read_http_request(stream) {
        Ok(request) => request,
        Err(error) => {
            append_http_diagnostic_log(
                "read_request",
                &DiagnosticContext {
                    remote_addr: peer_addr.map(|addr| addr.to_string()),
                    ..Default::default()
                },
                &error.to_string(),
            );
            return Err(error);
        }
    };
    let mut context = DiagnosticContext {
        remote_addr: peer_addr.map(|addr| addr.to_string()),
        path: Some(request.path.clone()),
        ..Default::default()
    };
    if let Err(error) = authorize_http_request(&request, config) {
        append_http_diagnostic_log("authorize", &context, &error.to_string());
        return Err(error);
    }
    if request.method != "POST" {
        let error = ServiceError::UnsupportedHttpMethod(request.method);
        append_http_diagnostic_log("method", &context, &error.to_string());
        return Err(error);
    }
    let rpc: JsonRpcRequest = match serde_json::from_slice(&request.body) {
        Ok(rpc) => rpc,
        Err(error) => {
            append_http_diagnostic_log("parse_json_rpc", &context, &error.to_string());
            return Err(error.into());
        }
    };
    let rpc_context = DiagnosticContext::from_rpc(&rpc);
    context.rpc_method = rpc_context.rpc_method;
    context.mcp_tool = rpc_context.mcp_tool;
    context.session_id = rpc_context.session_id;
    context.operation_id = rpc_context.operation_id;
    if rpc.jsonrpc != "2.0" {
        let error = ServiceError::Rpc("jsonrpc must be `2.0`".to_string());
        append_http_diagnostic_log("jsonrpc_version", &context, &error.to_string());
        return Err(error);
    }
    match request.path.as_str() {
        "/rpc" => {
            let response = http_json_response(200, &host.handle_rpc(rpc))?;
            append_http_success_log(200, started.elapsed(), &context);
            Ok(response)
        }
        "/mcp" => match host.handle_mcp(rpc) {
            Some(response) => {
                let response = http_json_response(200, &response)?;
                append_http_success_log(200, started.elapsed(), &context);
                Ok(response)
            }
            None => {
                let response = http_empty_response(202, "Accepted");
                append_http_success_log(202, started.elapsed(), &context);
                Ok(response)
            }
        },
        other => {
            let error = ServiceError::InvalidHttpRequest(format!("unsupported path `{other}`"));
            append_http_diagnostic_log("path", &context, &error.to_string());
            Err(error)
        }
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, ServiceError> {
    let mut buffer = Vec::new();
    let mut header_end = None;
    loop {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            header_end = Some(position + 4);
            break;
        }
        if buffer.len() > 64 * 1024 {
            return Err(ServiceError::InvalidHttpRequest(
                "request headers are too large".to_string(),
            ));
        }
    }
    let header_end = header_end.ok_or_else(|| {
        ServiceError::InvalidHttpRequest("request headers are incomplete".to_string())
    })?;
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| ServiceError::InvalidHttpRequest("missing request line".to_string()))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| ServiceError::InvalidHttpRequest("missing method".to_string()))?
        .to_string();
    let path = request_parts
        .next()
        .ok_or_else(|| ServiceError::InvalidHttpRequest("missing path".to_string()))?
        .to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_len = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_len {
        let mut chunk = vec![0u8; content_len - body.len()];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_len);
    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn authorize_http_request(
    request: &HttpRequest,
    config: &ServiceConfig,
) -> Result<(), ServiceError> {
    if !matches!(request.path.as_str(), "/rpc" | "/mcp") {
        return Err(ServiceError::InvalidHttpRequest(format!(
            "unsupported path `{}`",
            request.path
        )));
    }
    if let Some(origin) = request.headers.get("origin") {
        if !is_allowed_origin(origin) {
            return Err(ServiceError::InvalidOrigin(origin.clone()));
        }
    }
    let expected = format!("Bearer {}", config.bearer_token);
    match request.headers.get("authorization") {
        Some(actual) if actual == &expected => Ok(()),
        _ => Err(ServiceError::Unauthorized),
    }
}

fn http_json_response(status: u16, value: &JsonRpcResponse) -> Result<String, ServiceError> {
    let body = serde_json::to_string(value)?;
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        _ => "Bad Request",
    };
    Ok(format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    ))
}

fn http_empty_response(status: u16, reason: &str) -> String {
    format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn ensure_project_workspace(project_root: &Path) -> Result<Workspace, ServiceError> {
    let workspace_root = project_root.join(INTERNAL_WORKSPACE_DIR);
    match Workspace::open(&workspace_root) {
        Ok(workspace) => Ok(workspace),
        Err(WorkspaceError::ManifestNotFound(_)) => {
            Ok(Workspace::init(workspace_root, Default::default())?)
        }
        Err(error) => Err(error.into()),
    }
}

fn workspace_facts_with_fallback(path: &Path) -> Result<WorkspaceFacts, ServiceError> {
    match Workspace::open(path) {
        Ok(workspace) => return Ok(workspace.facts()?),
        Err(WorkspaceError::ManifestNotFound(_)) => {}
        Err(error) => return Err(error.into()),
    }

    let internal_workspace = path.join(INTERNAL_WORKSPACE_DIR);
    match Workspace::open(&internal_workspace) {
        Ok(workspace) => return Ok(workspace.facts()?),
        Err(WorkspaceError::ManifestNotFound(_)) => {}
        Err(error) => return Err(error.into()),
    }

    for artifacts_root in workspace_facts_artifact_candidates(path) {
        if artifacts_root.is_dir() {
            return fallback_workspace_facts_from_artifacts(&artifacts_root);
        }
    }

    Err(WorkspaceError::ManifestNotFound(path.join("dbgatlas-workspace.json")).into())
}

fn workspace_facts_artifact_candidates(path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if path
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("artifacts"))
    {
        candidates.push(path.to_path_buf());
    }
    candidates.push(path.join("artifacts"));
    candidates.push(path.join(INTERNAL_WORKSPACE_DIR).join("artifacts"));

    let mut unique = Vec::new();
    for candidate in candidates {
        if !unique.iter().any(|seen| seen == &candidate) {
            unique.push(candidate);
        }
    }
    unique
}

fn fallback_workspace_facts_from_artifacts(
    artifacts_root: &Path,
) -> Result<WorkspaceFacts, ServiceError> {
    let mut artifacts: Vec<ArtifactMetadata> =
        read_json_lines_optional(&artifacts_root.join("artifacts.jsonl"))?;
    if artifacts.is_empty() {
        artifacts = scan_synthetic_artifacts(artifacts_root)?;
    }
    Ok(WorkspaceFacts {
        artifacts,
        operations: read_json_lines_optional(&artifacts_root.join("operations.jsonl"))?,
        command_audit: read_json_lines_optional(&artifacts_root.join("command_audit.jsonl"))?,
    })
}

fn read_json_lines_optional<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, ServiceError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)?;
    let mut values = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        values.push(serde_json::from_str(line)?);
    }
    Ok(values)
}

fn scan_synthetic_artifacts(artifacts_root: &Path) -> Result<Vec<ArtifactMetadata>, ServiceError> {
    let workspace_root = artifacts_root.parent().unwrap_or(artifacts_root);
    let mut files = Vec::new();
    collect_artifact_files(artifacts_root, &mut files)?;
    files.sort();

    let mut artifacts = Vec::new();
    for (index, path) in files.into_iter().enumerate() {
        let relative_path = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .to_path_buf();
        let byte_len = fs::metadata(&path).ok().map(|metadata| metadata.len());
        artifacts.push(ArtifactMetadata {
            artifact_id: ArtifactRef::new(
                Id::new(format!("artifact-synthetic-{}", index + 1))
                    .expect("synthetic artifact ids are valid"),
            ),
            kind: infer_synthetic_artifact_kind(artifacts_root, &path),
            relative_path,
            created_at: Timestamp::now(),
            operation_id: None,
            byte_len,
            description: Some("discovered artifact (workspace manifest missing)".to_string()),
        });
    }
    Ok(artifacts)
}

fn collect_artifact_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), ServiceError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_artifact_files(&path, files)?;
        } else if file_type.is_file() && !is_workspace_index_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_workspace_index_file(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            matches!(
                name,
                "artifacts.jsonl" | "operations.jsonl" | "command_audit.jsonl"
            )
        })
}

fn infer_synthetic_artifact_kind(artifacts_root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(artifacts_root).unwrap_or(path);
    let components = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    match components.as_slice() {
        ["recordings", ..] if file_name == "recording.json" => "recording.metadata".to_string(),
        ["recordings", ..] if file_name == "events.jsonl" => "recording.events.ttd".to_string(),
        ["recordings", _, "events", event_file] => {
            let category = event_file.strip_suffix(".jsonl").unwrap_or(event_file);
            format!("recording.events.{category}")
        }
        ["recordings", ..] if file_name == "trace.etl" => "recording.trace".to_string(),
        ["recordings", ..] if extension == "run" => "recording.ttd.trace".to_string(),
        ["recordings", ..] if extension == "idx" => "recording.ttd.index".to_string(),
        ["recordings", ..] if file_name.starts_with("recorder") => {
            "recording.recorder_output".to_string()
        }
        ["sessions", ..] => "debug.artifact".to_string(),
        ["reverse_sessions", _, "core", ..] => "reverse.core".to_string(),
        ["reverse_sessions", _, "errors", ..] => "reverse.adapter_error".to_string(),
        ["reverse_sessions", ..] => "reverse.artifact".to_string(),
        _ => "artifact.file".to_string(),
    }
}

fn validate_config(config: &ServiceConfig) -> Result<(), ServiceError> {
    if !config.bind.ip().is_loopback() {
        return Err(ServiceError::NonLoopbackBind(config.bind));
    }
    if config.bearer_token.trim().is_empty() {
        return Err(ServiceError::EmptyBearerToken);
    }
    Ok(())
}

fn parse_params<T: for<'de> Deserialize<'de>>(params: Option<Value>) -> Result<T, ServiceError> {
    serde_json::from_value(params.unwrap_or(Value::Object(Default::default()))).map_err(Into::into)
}

fn parse_debug_session_create_params(
    params: Option<Value>,
) -> Result<DebugSessionCreateParams, ServiceError> {
    let value = params.unwrap_or(Value::Object(Default::default()));
    let ttd_target = value
        .get("target")
        .and_then(|target| target.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("ttd"));
    serde_json::from_value(value).map_err(|error| {
        if ttd_target {
            ServiceError::Rpc(
                "debug.session.create does not accept target kind `ttd`; use `{ \"kind\": \"file\", \"path\": \"trace.run\" }` for TTD .run replay"
                    .to_string(),
            )
        } else {
            error.into()
        }
    })
}

fn parse_u64_param(value: &Value, field: &'static str) -> Result<u64, ServiceError> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| ServiceError::Rpc(format!("{field} must be a non-negative integer"))),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Err(ServiceError::Rpc(format!("{field} must not be empty")));
            }
            if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16)
                    .map_err(|_| ServiceError::Rpc(format!("{field} is not a valid hex integer")))
            } else {
                text.parse::<u64>().map_err(|_| {
                    ServiceError::Rpc(format!("{field} is not a valid unsigned integer"))
                })
            }
        }
        _ => Err(ServiceError::Rpc(format!(
            "{field} must be an integer or integer string"
        ))),
    }
}

fn worker_failed_message(code: String, message: String) -> String {
    format!("{code}: {message}")
}

fn diagnose_reverse_open_error(message: String) -> String {
    if !message.contains("open_database failed with result")
        || message.contains("ida_error_kind=open_database_failed")
    {
        return message;
    }
    let Some(result) = open_database_result_code(&message) else {
        return format!(
            "{message}; ida_error_kind=open_database_failed; possible_reason=ida_rejected_database; suggestion=verify the database path, IDA version compatibility, file permissions, and whether another process has the database open"
        );
    };
    if result == 4 {
        format!(
            "{message}; ida_error_kind=open_database_failed; possible_reason=database_locked_or_unavailable; suggestion=close other IDA/IDALib sessions using this database, wait for file locks to release, or copy the IDB manually for review"
        )
    } else {
        format!(
            "{message}; ida_error_kind=open_database_failed; possible_reason=ida_rejected_database; suggestion=verify the database path, IDA version compatibility, file permissions, and whether another process has the database open"
        )
    }
}

fn open_database_result_code(message: &str) -> Option<i32> {
    let marker = "open_database failed with result";
    let tail = message.split(marker).nth(1)?;
    let digits = tail
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect::<String>();
    digits.parse().ok()
}

struct DebugStartFailure {
    code: String,
    message: String,
}

struct DebugSessionStartAttempt {
    worker: Option<WorkerHandle>,
    writes: Option<Vec<WorkerArtifactWrite>>,
    failures: Vec<String>,
}

impl std::fmt::Display for DebugStartFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

fn debug_start_failure_message(
    code: String,
    message: String,
    worker_identity: DebugWorkerIdentity,
) -> DebugStartFailure {
    DebugStartFailure {
        code,
        message: add_debug_access_denied_hint(message, worker_identity),
    }
}

fn should_auto_retry_active_interactive_user(
    requested_identity: DebugWorkerIdentity,
    target: &DebugTarget,
    failures: &[String],
) -> bool {
    requested_identity == DebugWorkerIdentity::Default
        && debug_target_can_need_active_interactive_user(target)
        && failures
            .iter()
            .any(|message| is_access_denied_message(message))
}

fn debug_target_can_need_active_interactive_user(target: &DebugTarget) -> bool {
    matches!(target, DebugTarget::File { .. }) || matches!(target, DebugTarget::Launch { .. })
}

fn debug_worker_identity_label(identity: Option<&WorkerIdentity>) -> &'static str {
    match identity {
        None => "default",
        Some(WorkerIdentity::LocalSystem) => "local_system",
        Some(WorkerIdentity::ActiveInteractiveUser) => "active_interactive_user",
        Some(WorkerIdentity::CurrentUserDevMode) => "current_user_dev_mode",
    }
}

fn add_debug_access_denied_hint(message: String, worker_identity: DebugWorkerIdentity) -> String {
    if worker_identity == DebugWorkerIdentity::Default && is_access_denied_message(&message) {
        format!(
            "{message}; recommended_retry_worker_identity=active_interactive_user; if this is an installed service using the default LocalSystem debug worker, retry debug.session.create with worker_identity:\"active_interactive_user\" for user-session dump files, TTD replay, or live launch"
        )
    } else {
        message
    }
}

fn is_access_denied_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("access is denied")
        || lower.contains("os error 5")
        || lower.contains("0x80070005")
        || lower.contains("e_accessdenied")
        || message.contains("拒绝访问")
}

fn validate_optional_timeout_ms(timeout_ms: Option<u64>) -> Result<(), ServiceError> {
    if timeout_ms == Some(0) {
        return Err(ServiceError::Rpc(
            "timeout_ms must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn session_relative_path(session_id: &SessionRef, suffix: &str) -> PathBuf {
    PathBuf::from("artifacts")
        .join("sessions")
        .join(session_id.id.as_str())
        .join(suffix)
}

fn recording_relative_path(recording_id: &RecordingRef, suffix: &str) -> PathBuf {
    PathBuf::from("artifacts")
        .join("recordings")
        .join(recording_id.id.as_str())
        .join(suffix)
}

fn reverse_relative_path(session_id: &SessionRef, suffix: &str) -> PathBuf {
    PathBuf::from("artifacts")
        .join("reverse_sessions")
        .join(session_id.id.as_str())
        .join(suffix)
}

fn etw_session_name(recording_id: &RecordingRef) -> String {
    format!("DbgAtlas-{}", recording_id.id.as_str())
}

fn next_session_ref() -> SessionRef {
    let count = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    SessionRef::new(
        Id::new(format!("session-{}-{count}", Timestamp::now().unix_millis))
            .expect("generated session ids are valid"),
    )
}

fn next_recording_ref() -> RecordingRef {
    let count = RECORDING_COUNTER.fetch_add(1, Ordering::Relaxed);
    RecordingRef::new(
        Id::new(format!(
            "recording-{}-{count}",
            Timestamp::now().unix_millis
        ))
        .expect("generated recording ids are valid"),
    )
}

fn next_operation_ref() -> OperationRef {
    let count = OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    OperationRef::new(
        Id::new(format!("op-{}-{count}", Timestamp::now().unix_millis))
            .expect("generated operation ids are valid"),
    )
}

fn next_artifact_ref() -> ArtifactRef {
    let count = ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed);
    ArtifactRef::new(
        Id::new(format!("artifact-{}-{count}", Timestamp::now().unix_millis))
            .expect("generated artifact ids are valid"),
    )
}

fn rpc_error_for(error: ServiceError) -> JsonRpcError {
    let code = match error {
        ServiceError::Unauthorized => -32001,
        ServiceError::InvalidOrigin(_) => -32002,
        ServiceError::SessionNotFound(_) => -32010,
        ServiceError::OperationNotFound(_) => -32011,
        ServiceError::SessionNotReusable(_) => -32012,
        ServiceError::SessionAlreadyTerminal(_) => -32013,
        ServiceError::OperationNotCancelable(_) => -32014,
        ServiceError::RecordingNotFound(_) => -32015,
        ServiceError::RecordingAlreadyTerminal(_) => -32016,
        ServiceError::UnsupportedHttpMethod(_) => -32600,
        ServiceError::Rpc(_) | ServiceError::Json(_) => -32602,
        _ => -32000,
    };
    JsonRpcError {
        code,
        message: error.to_string(),
    }
}

fn mcp_error_for(error: ServiceError) -> JsonRpcError {
    JsonRpcError {
        code: -32000,
        message: error.to_string(),
    }
}

fn http_status_for(error: &ServiceError) -> u16 {
    match error {
        ServiceError::Unauthorized => 401,
        ServiceError::InvalidOrigin(_) => 403,
        ServiceError::UnsupportedHttpMethod(_) => 405,
        _ => 400,
    }
}

fn is_allowed_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    if rest.contains('/') {
        return false;
    }

    let host = if let Some(rest) = rest.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return false;
        };
        if !tail.is_empty() && !tail.starts_with(':') {
            return false;
        }
        host
    } else {
        rest.split_once(':').map_or(rest, |(host, _port)| host)
    };

    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|candidate| candidate == needle)
}

mod job {
    #[cfg(windows)]
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    #[cfg(windows)]
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    #[cfg(windows)]
    pub struct ManagedJob {
        handle: HANDLE,
    }

    unsafe impl Send for ManagedJob {}
    unsafe impl Sync for ManagedJob {}

    #[cfg(windows)]
    impl ManagedJob {
        pub fn create(_name: &str) -> Self {
            Self::create_result(_name).unwrap_or(Self {
                handle: std::ptr::null_mut(),
            })
        }

        pub fn create_result(_name: &str) -> Result<Self, std::io::Error> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                unsafe {
                    CloseHandle(handle);
                }
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        pub fn assign_process(&self, process: &crate::WorkerProcess) -> Result<(), std::io::Error> {
            use std::os::windows::io::AsRawHandle;

            if self.handle.is_null() {
                return Ok(());
            }
            let process_handle = match process {
                crate::WorkerProcess::Std(child) => child.as_raw_handle() as HANDLE,
                crate::WorkerProcess::RawWindows(process) => process.handle(),
            };
            let ok = unsafe { AssignProcessToJobObject(self.handle, process_handle) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn assign_child_process(
            &self,
            process: &std::process::Child,
        ) -> Result<(), std::io::Error> {
            use std::os::windows::io::AsRawHandle;

            if self.handle.is_null() {
                return Ok(());
            }
            let ok =
                unsafe { AssignProcessToJobObject(self.handle, process.as_raw_handle() as HANDLE) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }

    #[cfg(windows)]
    impl Drop for ManagedJob {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe {
                    CloseHandle(self.handle);
                }
            }
        }
    }

    #[cfg(not(windows))]
    pub struct ManagedJob;

    #[cfg(not(windows))]
    impl ManagedJob {
        pub fn create(_name: &str) -> Self {
            Self
        }

        pub fn create_result(_name: &str) -> Result<Self, std::io::Error> {
            Ok(Self)
        }

        pub fn assign_process(
            &self,
            _process: &crate::WorkerProcess,
        ) -> Result<(), std::io::Error> {
            Ok(())
        }

        pub fn assign_child_process(
            &self,
            _process: &std::process::Child,
        ) -> Result<(), std::io::Error> {
            Ok(())
        }
    }
}

#[cfg(windows)]
mod windows_active_user_process {
    use super::{ServiceError, WINDOWS_SERVICE_NAME};
    use std::ffi::{OsStr, OsString, c_void};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE, WAIT_FAILED};
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, TOKEN_ALL_ACCESS,
        TOKEN_ELEVATION_TYPE, TOKEN_LINKED_TOKEN, TokenElevationType, TokenElevationTypeLimited,
        TokenLinkedToken, TokenPrimary,
    };
    use windows_sys::Win32::System::Environment::{
        CreateEnvironmentBlock, DestroyEnvironmentBlock,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSGetActiveConsoleSessionId, WTSQueryUserToken,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetExitCodeProcess, INFINITE,
        PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
    };

    pub struct RawProcess {
        process_handle: HANDLE,
        thread_handle: HANDLE,
        exit_code: Option<i32>,
        waited: bool,
    }

    unsafe impl Send for RawProcess {}

    impl RawProcess {
        pub fn handle(&self) -> HANDLE {
            self.process_handle
        }

        pub fn kill(&mut self) -> Result<(), io::Error> {
            if self.process_handle.is_null() || self.exit_code.is_some() {
                return Ok(());
            }
            let mut exit_code = 0;
            let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
            if ok != 0 && exit_code != STILL_ACTIVE as u32 {
                self.exit_code = Some(exit_code as i32);
                return Ok(());
            }
            let ok = unsafe { TerminateProcess(self.process_handle, 1) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn try_wait(&mut self) -> Result<Option<Option<i32>>, io::Error> {
            if let Some(exit_code) = self.exit_code {
                return Ok(Some(Some(exit_code)));
            }
            if self.process_handle.is_null() {
                return Ok(Some(None));
            }
            let mut exit_code = 0;
            let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if exit_code == STILL_ACTIVE as u32 {
                return Ok(None);
            }
            let exit_code = exit_code as i32;
            self.exit_code = Some(exit_code);
            self.waited = true;
            Ok(Some(Some(exit_code)))
        }

        pub fn wait_code(&mut self) -> Result<Option<i32>, io::Error> {
            if let Some(exit_code) = self.exit_code {
                return Ok(Some(exit_code));
            }
            if self.process_handle.is_null() {
                return Ok(None);
            }
            if !self.waited {
                let status = unsafe { WaitForSingleObject(self.process_handle, INFINITE) };
                if status == WAIT_FAILED {
                    return Err(io::Error::last_os_error());
                }
                self.waited = true;
            }
            let mut exit_code = 0;
            let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let exit_code = exit_code as i32;
            self.exit_code = Some(exit_code);
            Ok(Some(exit_code))
        }

        pub fn wait(&mut self) -> Result<(), io::Error> {
            self.wait_code().map(|_| ())
        }
    }

    impl Drop for RawProcess {
        fn drop(&mut self) {
            unsafe {
                if !self.thread_handle.is_null() {
                    CloseHandle(self.thread_handle);
                }
                if !self.process_handle.is_null() {
                    CloseHandle(self.process_handle);
                }
            }
        }
    }

    struct Handle(HANDLE);

    impl Handle {
        fn new(handle: HANDLE) -> Self {
            Self(handle)
        }

        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    struct EnvironmentBlock(*mut c_void);

    impl EnvironmentBlock {
        fn create(primary_token: HANDLE) -> Result<Self, ServiceError> {
            let mut environment = std::ptr::null_mut();
            let ok = unsafe { CreateEnvironmentBlock(&mut environment, primary_token, 0) };
            if ok == 0 {
                return Err(ServiceError::Worker(format!(
                    "CreateEnvironmentBlock for active interactive user failed: {}",
                    io::Error::last_os_error()
                )));
            }
            Ok(Self(environment))
        }

        fn raw(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for EnvironmentBlock {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DestroyEnvironmentBlock(self.0);
                }
            }
        }
    }

    pub fn spawn(worker_exe: &Path, args: &[String]) -> Result<RawProcess, ServiceError> {
        spawn_impl(worker_exe, args.iter().map(|arg| OsStr::new(arg)), "worker")
    }

    pub fn spawn_os(executable: &Path, args: &[OsString]) -> Result<RawProcess, ServiceError> {
        spawn_impl(
            executable,
            args.iter().map(|arg| arg.as_os_str()),
            "process",
        )
    }

    fn spawn_impl<'a, I>(
        executable: &Path,
        args: I,
        process_label: &str,
    ) -> Result<RawProcess, ServiceError>
    where
        I: IntoIterator<Item = &'a OsStr>,
    {
        let session_id = unsafe { WTSGetActiveConsoleSessionId() };
        if session_id == u32::MAX {
            return Err(ServiceError::Worker(
                "no active interactive session is available for active interactive process"
                    .to_string(),
            ));
        }

        let mut impersonation_token = std::ptr::null_mut();
        let ok = unsafe { WTSQueryUserToken(session_id, &mut impersonation_token) };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "WTSQueryUserToken for active interactive session {session_id} failed: {}",
                io::Error::last_os_error()
            )));
        }
        let impersonation_token = Handle::new(impersonation_token);

        let primary_token = primary_token_for_active_session(&impersonation_token, session_id)?;
        let environment = EnvironmentBlock::create(primary_token.raw())?;

        let mut command_line = command_line(executable.as_os_str(), args);
        let mut desktop = wide_null("winsta0\\default");
        let current_directory = executable
            .parent()
            .map(|path| path.as_os_str())
            .unwrap_or_else(|| OsStr::new("."));
        let current_directory = wide_null_os(current_directory);
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        startup.lpDesktop = desktop.as_mut_ptr();
        let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessAsUserW(
                primary_token.raw(),
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_UNICODE_ENVIRONMENT,
                environment.raw(),
                current_directory.as_ptr(),
                &startup,
                &mut process_info,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "CreateProcessAsUserW failed to launch {WINDOWS_SERVICE_NAME} {process_label} in active interactive session {session_id}: {}",
                io::Error::last_os_error()
            )));
        }

        Ok(RawProcess {
            process_handle: process_info.hProcess,
            thread_handle: process_info.hThread,
            exit_code: None,
            waited: false,
        })
    }

    fn primary_token_for_active_session(
        user_token: &Handle,
        session_id: u32,
    ) -> Result<Handle, ServiceError> {
        if token_elevation_type(user_token.raw(), session_id)? == TokenElevationTypeLimited {
            let linked_token = linked_elevated_token(user_token.raw(), session_id)?;
            return duplicate_primary_token(
                linked_token.raw(),
                &format!("elevated linked token for active interactive session {session_id}"),
            );
        }
        duplicate_primary_token(
            user_token.raw(),
            &format!("active interactive session {session_id}"),
        )
    }

    fn token_elevation_type(
        token: HANDLE,
        session_id: u32,
    ) -> Result<TOKEN_ELEVATION_TYPE, ServiceError> {
        let mut elevation_type: TOKEN_ELEVATION_TYPE = 0;
        let mut return_length = 0;
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenElevationType,
                &mut elevation_type as *mut _ as *mut c_void,
                std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
                &mut return_length,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "GetTokenInformation(TokenElevationType) for active interactive session {session_id} failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(elevation_type)
    }

    fn linked_elevated_token(token: HANDLE, session_id: u32) -> Result<Handle, ServiceError> {
        let mut linked: TOKEN_LINKED_TOKEN = unsafe { std::mem::zeroed() };
        let mut return_length = 0;
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenLinkedToken,
                &mut linked as *mut _ as *mut c_void,
                std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32,
                &mut return_length,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "GetTokenInformation(TokenLinkedToken) for active interactive session {session_id} failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Handle::new(linked.LinkedToken))
    }

    fn duplicate_primary_token(token: HANDLE, context: &str) -> Result<Handle, ServiceError> {
        let mut primary_token = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateTokenEx(
                token,
                TOKEN_ALL_ACCESS,
                std::ptr::null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut primary_token,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "DuplicateTokenEx for {context} failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Handle::new(primary_token))
    }

    fn command_line<'a, I>(executable: &OsStr, args: I) -> Vec<u16>
    where
        I: IntoIterator<Item = &'a OsStr>,
    {
        let args = args.into_iter();
        let (lower_bound, _) = args.size_hint();
        let mut parts = Vec::with_capacity(lower_bound + 1);
        parts.push(quote_arg(&executable.to_string_lossy()));
        parts.extend(args.map(|arg| quote_arg(&arg.to_string_lossy())));
        wide_null(&parts.join(" "))
    }

    fn wide_null(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_null_os(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn quote_arg(arg: &str) -> String {
        if arg.is_empty() {
            return "\"\"".to_string();
        }
        let needs_quotes = arg
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'));
        if !needs_quotes {
            return arg.to_string();
        }
        let mut quoted = String::from("\"");
        let mut backslashes = 0;
        for ch in arg.chars() {
            match ch {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                _ => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes));
                    backslashes = 0;
                    quoted.push(ch);
                }
            }
        }
        quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        quoted.push('"');
        quoted
    }
}

#[cfg(windows)]
mod suspended_process {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CreateProcessW, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW,
        TerminateProcess,
    };

    pub struct SuspendedProcess {
        process_handle: HANDLE,
        thread_handle: HANDLE,
        pid: u32,
        resumed: bool,
    }

    impl SuspendedProcess {
        pub fn create(executable: &Path, args: &[String]) -> Result<Self, io::Error> {
            let mut command_line = command_line(executable.as_os_str(), args);
            let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                CreateProcessW(
                    std::ptr::null(),
                    command_line.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    CREATE_SUSPENDED,
                    std::ptr::null(),
                    std::ptr::null(),
                    &mut startup,
                    &mut process_info,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                process_handle: process_info.hProcess,
                thread_handle: process_info.hThread,
                pid: process_info.dwProcessId,
                resumed: false,
            })
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn resume(mut self) -> Result<(), io::Error> {
            let previous = unsafe { ResumeThread(self.thread_handle) };
            if previous == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            self.resumed = true;
            Ok(())
        }
    }

    impl Drop for SuspendedProcess {
        fn drop(&mut self) {
            if !self.resumed && !self.process_handle.is_null() {
                unsafe {
                    TerminateProcess(self.process_handle, 1);
                }
            }
            unsafe {
                if !self.thread_handle.is_null() {
                    CloseHandle(self.thread_handle);
                }
                if !self.process_handle.is_null() {
                    CloseHandle(self.process_handle);
                }
            }
        }
    }

    fn command_line(executable: &OsStr, args: &[String]) -> Vec<u16> {
        let mut parts = Vec::with_capacity(args.len() + 1);
        parts.push(quote_arg(&executable.to_string_lossy()));
        parts.extend(args.iter().map(|arg| quote_arg(arg)));
        OsStr::new(&parts.join(" "))
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn quote_arg(arg: &str) -> String {
        if arg.is_empty() {
            return "\"\"".to_string();
        }
        let needs_quotes = arg
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'));
        if !needs_quotes {
            return arg.to_string();
        }

        let mut quoted = String::from("\"");
        let mut backslashes = 0;
        for ch in arg.chars() {
            if ch == '\\' {
                backslashes += 1;
                continue;
            }
            if ch == '"' {
                quoted.extend(std::iter::repeat('\\').take(backslashes * 2 + 1));
                quoted.push('"');
            } else {
                quoted.extend(std::iter::repeat('\\').take(backslashes));
                quoted.push(ch);
            }
            backslashes = 0;
        }
        quoted.extend(std::iter::repeat('\\').take(backslashes * 2));
        quoted.push('"');
        quoted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    #[test]
    fn service_paths_are_rooted_under_dbgatlas_install_dir() {
        let root = PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas");
        let paths = ServiceInstallPaths::for_root(root.clone());

        assert_eq!(paths.root_dir, root);
        assert_eq!(
            paths.bin_dir,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin")
        );
        assert_eq!(
            paths.etc_dir,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\etc")
        );
        assert_eq!(
            paths.rt_dir,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin\rt")
        );
        assert_eq!(
            paths.windbg_runtime_dir,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin\rt\windbg")
                .join(windbg_runtime_arch())
        );
        assert_eq!(
            paths.var_dir,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\var")
        );
        assert_eq!(
            paths.log_dir,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\var\log")
        );
        assert_eq!(
            paths.installed_exe,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin\dbgatlas.exe")
        );
        assert_eq!(
            paths.config_path,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\etc\runtime.toml")
        );
        assert_eq!(
            paths.token_file,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\etc\token")
        );
    }

    #[test]
    fn service_paths_can_be_inferred_from_installed_executable() {
        let executable =
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin\dbgatlas.exe");
        let paths = service_paths_from_executable_path(&executable).unwrap();

        assert_eq!(
            paths.root_dir,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas")
        );
        assert_eq!(paths.installed_exe, executable);
        assert_eq!(
            paths.config_path,
            PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\etc\runtime.toml")
        );
    }

    #[test]
    fn service_paths_can_be_inferred_from_quoted_scm_command_line_with_install_root() {
        let command_line = PathBuf::from(
            r#""C:\Users\d stars\AppData\Local\Programs\dbgatlas\bin\dbgatlas.exe" service run --windows-service --install-root "C:\Users\d stars\AppData\Local\Programs\dbgatlas" --config "C:\ignored\etc\runtime.toml" --token-file "C:\ignored\etc\token""#,
        );

        let paths = service_paths_from_scm_binary_path(&command_line).unwrap();

        assert_eq!(
            paths.root_dir,
            PathBuf::from(r"C:\Users\d stars\AppData\Local\Programs\dbgatlas")
        );
        assert_eq!(
            paths.installed_exe,
            PathBuf::from(r"C:\Users\d stars\AppData\Local\Programs\dbgatlas\bin\dbgatlas.exe")
        );
    }

    #[test]
    fn service_paths_can_be_inferred_from_legacy_scm_config_argument() {
        let command_line = PathBuf::from(
            r#""C:\ProgramData\DbgAtlas\bin\dbgatlas.exe" service run --windows-service --config "C:\ProgramData\DbgAtlas\etc\runtime.toml" --token-file "C:\ProgramData\DbgAtlas\etc\token""#,
        );

        let paths = service_paths_from_scm_binary_path(&command_line).unwrap();

        assert_eq!(paths.root_dir, PathBuf::from(r"C:\ProgramData\DbgAtlas"));
        assert_eq!(
            paths.installed_exe,
            PathBuf::from(r"C:\ProgramData\DbgAtlas\bin\dbgatlas.exe")
        );
    }

    #[test]
    fn service_command_line_split_preserves_quoted_arguments() {
        let args = split_windows_command_line(
            r#""C:\Program Files\dbgatlas\dbgatlas.exe" --install-root "C:\Users\d stars\AppData\Local\Programs\dbgatlas" --empty """#,
        );

        assert_eq!(
            args,
            vec![
                r"C:\Program Files\dbgatlas\dbgatlas.exe",
                "--install-root",
                r"C:\Users\d stars\AppData\Local\Programs\dbgatlas",
                "--empty",
                "",
            ]
        );
    }

    #[test]
    fn payload_discovery_requires_all_runtime_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("dbgatlas.exe"), "").unwrap();
        fs::write(temp.path().join("dbgatlas-worker.exe"), "").unwrap();

        let error = discover_service_payload(temp.path(), &temp.path().join("bin")).unwrap_err();

        assert!(matches!(error, ServiceError::IncompleteInstallPayload(_)));
        assert!(error.to_string().contains("dbgatlas_dbgeng.dll"));
    }

    #[test]
    fn payload_discovery_maps_sources_to_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("install-bin");
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(temp.path().join(file_name), "").unwrap();
        }

        let payload = discover_service_payload(temp.path(), &destination).unwrap();

        assert_eq!(payload.len(), WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len());
        assert!(payload.iter().any(|file| {
            file.file_name == "dbgatlas-worker.exe"
                && file.destination == destination.join("dbgatlas-worker.exe")
        }));
        assert!(payload.iter().any(|file| {
            file.file_name == "dbgatlas_dbgeng.dll"
                && file.destination == destination.join("dbgatlas_dbgeng.dll")
        }));
    }

    #[test]
    fn payload_discovery_ignores_legacy_mingw_runtime_files() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("install-bin");
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(temp.path().join(file_name), "").unwrap();
        }
        for file_name in [
            "libgcc_s_seh-1.dll",
            "libstdc++-6.dll",
            "libwinpthread-1.dll",
        ] {
            fs::write(temp.path().join(file_name), "").unwrap();
        }

        let payload = discover_service_payload(temp.path(), &destination).unwrap();

        assert_eq!(payload.len(), WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len());
        for file_name in [
            "libgcc_s_seh-1.dll",
            "libstdc++-6.dll",
            "libwinpthread-1.dll",
        ] {
            assert!(!payload.iter().any(|file| file.file_name == file_name));
        }
    }

    #[test]
    fn use_existing_payload_validates_installed_bin_without_overwriting_files() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.bin_dir).unwrap();
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(paths.bin_dir.join(file_name), file_name).unwrap();
        }
        fs::write(paths.bin_dir.join("dbgatlas.exe"), "existing-exe").unwrap();

        let payload = validate_existing_payload(&paths).unwrap();

        assert_eq!(payload.len(), WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len());
        assert_eq!(
            fs::read_to_string(paths.bin_dir.join("dbgatlas.exe")).unwrap(),
            "existing-exe"
        );
        assert!(payload.iter().any(|file| {
            file.source == paths.bin_dir.join("dbgatlas-worker.exe")
                && file.destination == paths.bin_dir.join("dbgatlas-worker.exe")
        }));
    }

    #[test]
    fn copy_payload_installs_files_into_bin() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("payload");
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&source).unwrap();
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(source.join(file_name), file_name).unwrap();
        }
        let payload = discover_service_payload(&source, &paths.bin_dir).unwrap();

        install_payload(&payload, &paths).unwrap();

        assert_eq!(
            fs::read_to_string(paths.bin_dir.join("dbgatlas.exe")).unwrap(),
            "dbgatlas.exe"
        );
        assert!(paths.bin_dir.join("dbgatlas_ida.dll").is_file());
    }

    #[test]
    fn service_update_rejects_incomplete_payload() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::write(temp.path().join("dbgatlas.exe"), "").unwrap();

        let error = prepare_service_update(temp.path(), &paths, true).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("service install payload is incomplete")
        );
    }

    #[test]
    fn service_update_rejects_installed_bin_source() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.bin_dir).unwrap();

        let error = prepare_service_update(&paths.bin_dir, &paths, true).unwrap_err();

        assert!(matches!(error, ServiceError::ServiceControl(_)));
        assert!(error.to_string().contains("installed bin directory"));
    }

    #[test]
    fn service_update_accepted_response_maps_payload() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("payload");
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&source).unwrap();
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(source.join(file_name), file_name).unwrap();
        }

        let prepared = prepare_service_update(&source, &paths, false).unwrap();

        assert_eq!(prepared.response.status, "accepted");
        assert_eq!(prepared.response.restart, false);
        assert_eq!(prepared.response.service_name, WINDOWS_SERVICE_NAME);
        assert_eq!(prepared.response.installed_binary, paths.installed_exe);
        assert_eq!(prepared.response.log_dir, paths.log_dir);
        assert_eq!(
            prepared.response.source_dir,
            fs::canonicalize(&source).unwrap()
        );
        assert_eq!(
            prepared.response.payload.len(),
            WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len()
        );
        assert!(prepared.response.payload.iter().any(|file| {
            file.file_name == "dbgatlas.exe"
                && file.destination == paths.bin_dir.join("dbgatlas.exe")
        }));
    }

    #[test]
    fn service_update_can_replace_runtime_config_but_not_token() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("payload");
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&paths.etc_dir).unwrap();
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(source.join(file_name), file_name).unwrap();
        }
        fs::write(
            source.join(WINDOWS_SERVICE_CONFIG_FILE),
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n",
        )
        .unwrap();
        fs::write(source.join(WINDOWS_SERVICE_TOKEN_FILE), "payload-token\n").unwrap();
        fs::write(
            &paths.config_path,
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7331\"\n",
        )
        .unwrap();
        fs::write(&paths.token_file, "installed-token\n").unwrap();

        let prepared = prepare_service_update(&source, &paths, false).unwrap();
        let config_payload = prepared
            .config_payload
            .as_ref()
            .expect("runtime config payload is discovered");

        assert_eq!(config_payload.destination, paths.config_path);
        assert!(
            prepared
                .response
                .payload
                .iter()
                .any(|file| file.destination == paths.config_path)
        );
        assert!(
            !prepared
                .response
                .payload
                .iter()
                .any(|file| file.destination == paths.token_file)
        );

        let staging = temp.path().join("config-staging");
        let staged_payload =
            copy_update_config_to_staging(config_payload, &staging, &paths).unwrap();
        assert!(staged_payload.source.starts_with(&staging));

        apply_update_config_payload(&staged_payload, &paths).unwrap();

        assert!(
            fs::read_to_string(&paths.config_path)
                .unwrap()
                .contains("127.0.0.1:7444")
        );
        assert_eq!(
            fs::read_to_string(&paths.token_file).unwrap(),
            "installed-token\n"
        );
    }

    #[test]
    fn service_update_rejects_token_fields_in_runtime_config() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("payload");
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&source).unwrap();
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(source.join(file_name), file_name).unwrap();
        }
        fs::write(
            source.join(WINDOWS_SERVICE_CONFIG_FILE),
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\ntoken = \"secret\"\n",
        )
        .unwrap();

        let error = prepare_service_update(&source, &paths, false).unwrap_err();

        assert!(error.to_string().contains("must not contain token fields"));
        assert!(error.to_string().contains("server.token"));
    }

    #[test]
    fn service_update_rejects_removed_local_debug_tool_fields() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("payload");
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&source).unwrap();
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(source.join(file_name), file_name).unwrap();
        }
        fs::write(
            source.join(WINDOWS_SERVICE_CONFIG_FILE),
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n\n[tools]\ndbgeng_dir = \"C:\\\\stale\\\\windbg\"\n",
        )
        .unwrap();

        let error = prepare_service_update(&source, &paths, false).unwrap_err();

        assert!(error.to_string().contains("removed local debug tool field"));
        assert!(error.to_string().contains("tools.dbgeng_dir"));
    }

    #[test]
    fn service_update_replace_helper_swaps_and_cleans_update_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        let staging = paths.root_dir.join("bin.next-test");
        fs::create_dir_all(&paths.bin_dir).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(paths.bin_dir.join("dbgatlas.exe"), "old").unwrap();
        fs::write(staging.join("dbgatlas.exe"), "new").unwrap();

        let old_dir =
            replace_installed_bin_with_staging(&paths, &staging, "test", Duration::from_secs(1))
                .unwrap();

        assert_eq!(
            fs::read_to_string(paths.bin_dir.join("dbgatlas.exe")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(old_dir.join("dbgatlas.exe")).unwrap(),
            "old"
        );
        cleanup_update_dirs(&paths).unwrap();
        assert!(paths.bin_dir.is_dir());
        assert!(!old_dir.exists());
        assert!(!staging.exists());
    }

    #[test]
    fn detects_installing_from_installed_bin_directory() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.bin_dir).unwrap();

        assert!(source_is_installed_bin(&paths.bin_dir, &paths));
    }

    #[test]
    fn installed_client_config_reads_runtime_config_and_token() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.bin_dir).unwrap();
        fs::create_dir_all(&paths.etc_dir).unwrap();
        fs::write(&paths.installed_exe, "").unwrap();
        fs::write(
            &paths.config_path,
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n",
        )
        .unwrap();
        fs::write(&paths.token_file, "installed-token\n").unwrap();

        let config = installed_client_config_from_paths(&paths).unwrap().unwrap();

        assert_eq!(config.bind, "127.0.0.1:7444".parse().unwrap());
        assert_eq!(config.bearer_token, "installed-token");
    }

    #[test]
    fn service_capabilities_read_ida_py_eval_runtime_policy() {
        let runtime = RuntimeConfig::from_toml_str(
            r#"
version = 1

[tools.ida]
allow_py_eval = true
"#,
        )
        .unwrap();

        let capabilities = ServiceCapabilities::from_runtime_config(&runtime);

        assert!(capabilities.ida_py_eval);
    }

    #[test]
    fn install_runtime_config_does_not_persist_auto_debug_stack() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.root_dir).unwrap();

        let runtime =
            create_runtime_config_if_missing(&paths, "127.0.0.1:7331".parse().unwrap()).unwrap();
        let config = fs::read_to_string(&paths.config_path).unwrap();

        assert_eq!(runtime.server.bind, "127.0.0.1:7331".parse().unwrap());
        assert!(!config.contains("dbgeng_dir"));
        assert!(!config.contains("ttd_dir"));
        assert!(!config.contains("WindowsApps"));
    }

    #[test]
    fn installed_client_config_ignores_stale_config_without_installed_binary() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.etc_dir).unwrap();
        fs::write(
            &paths.config_path,
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n",
        )
        .unwrap();
        fs::write(&paths.token_file, "installed-token\n").unwrap();

        assert!(
            installed_client_config_from_paths(&paths)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn runtime_config_creation_rejects_non_loopback_bind() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.root_dir).unwrap();

        let error =
            create_runtime_config_if_missing(&paths, "0.0.0.0:7331".parse().unwrap()).unwrap_err();

        assert!(matches!(
            error,
            ServiceError::Runtime(dbgatlas_runtime::RuntimeConfigError::NonLoopbackBind(_))
        ));
        assert!(!paths.config_path.exists());
    }

    #[test]
    fn install_layout_migrates_legacy_config_and_token() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.root_dir).unwrap();
        fs::write(
            paths.legacy_config_path(),
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n",
        )
        .unwrap();
        fs::write(paths.legacy_token_file(), "legacy-token\n").unwrap();

        prepare_install_layout(&paths).unwrap();

        assert!(paths.etc_dir.is_dir());
        assert!(paths.log_dir.is_dir());
        assert!(paths.install_marker_path().is_file());
        assert_eq!(
            fs::read_to_string(&paths.config_path).unwrap(),
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n"
        );
        assert_eq!(
            fs::read_to_string(&paths.token_file).unwrap(),
            "legacy-token\n"
        );
        assert!(!paths.legacy_config_path().exists());
        assert!(!paths.legacy_token_file().exists());
    }

    #[test]
    fn install_state_copy_preserves_previous_token_when_new_etc_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let previous = ServiceInstallPaths::for_root(temp.path().join("old"));
        let paths = ServiceInstallPaths::for_root(temp.path().join("new"));
        fs::create_dir_all(&previous.etc_dir).unwrap();
        fs::create_dir_all(&paths.etc_dir).unwrap();
        fs::write(
            &previous.config_path,
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7331\"\n",
        )
        .unwrap();
        fs::write(&previous.token_file, "previous-token\n").unwrap();

        copy_missing_install_state_from_paths(&paths, &previous).unwrap();

        assert_eq!(
            fs::read_to_string(&paths.config_path).unwrap(),
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7331\"\n"
        );
        assert_eq!(
            fs::read_to_string(&paths.token_file).unwrap(),
            "previous-token\n"
        );
    }

    #[test]
    fn cleanup_install_dirs_only_removes_files_when_purging() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        fs::create_dir_all(&paths.bin_dir).unwrap();
        fs::write(paths.bin_dir.join("dbgatlas.exe"), "payload").unwrap();

        cleanup_install_dirs(&paths, false).unwrap();

        assert!(paths.bin_dir.join("dbgatlas.exe").is_file());

        fs::write(paths.install_marker_path(), "DbgAtlas install root\n").unwrap();
        cleanup_install_dirs(&paths, true).unwrap();

        assert!(!paths.root_dir.exists());
    }

    #[test]
    fn cleanup_install_dirs_rejects_unmarked_custom_root_when_purging() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("custom-root"));
        fs::create_dir_all(&paths.bin_dir).unwrap();
        fs::write(paths.bin_dir.join("dbgatlas.exe"), "payload").unwrap();

        let error = cleanup_install_dirs(&paths, true).unwrap_err();

        assert!(error.to_string().contains("refusing to purge install root"));
        assert!(paths.root_dir.exists());
    }

    #[test]
    fn cleanup_install_dirs_rejects_unmarked_dbgatlas_named_root_when_purging() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("dbgatlas"));
        fs::create_dir_all(&paths.bin_dir).unwrap();
        fs::write(paths.bin_dir.join("dbgatlas.exe"), "payload").unwrap();

        let error = cleanup_install_dirs(&paths, true).unwrap_err();

        assert!(error.to_string().contains("refusing to purge install root"));
        assert!(paths.root_dir.exists());
    }

    #[test]
    fn cleanup_install_dirs_allows_marked_custom_root_when_purging() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("custom-root"));
        fs::create_dir_all(&paths.bin_dir).unwrap();
        fs::write(paths.install_marker_path(), "DbgAtlas install root\n").unwrap();

        cleanup_install_dirs(&paths, true).unwrap();

        assert!(!paths.root_dir.exists());
    }

    #[cfg(windows)]
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

    #[cfg(windows)]
    #[test]
    fn service_log_file_day_rejects_invalid_dates() {
        assert!(windows_service_control::service_log_file_day("service-2026-02-29.log").is_none());
        assert!(windows_service_control::service_log_file_day("service-2024-02-29.log").is_some());
        assert!(windows_service_control::service_log_file_day("service-latest.log").is_none());
    }

    #[test]
    fn health_rpc_returns_ok() {
        let host = ServiceHost::with_mock_workers();
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "service.health".to_string(),
            params: None,
        });

        assert!(response.error.is_none());
        assert_eq!(response.result.unwrap()["status"], "ok");
    }

    #[test]
    fn create_session_uses_project_root_internal_dbgatlas_dir() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let response = create_debug_session(&host, temp.path());

        assert!(response.error.is_none(), "{:?}", response.error);
        assert!(
            temp.path()
                .join(INTERNAL_WORKSPACE_DIR)
                .join("dbgatlas-workspace.json")
                .is_file()
        );
        let result = response.result.unwrap();
        assert!(result.get("session_id").is_some());
        assert!(result.get("worker_binding").is_none());
        assert_eq!(result["operation_status"], "success");
        assert!(result["artifact_refs"].as_array().unwrap().is_empty());
        assert!(result["raw_output_ref"].is_null());
    }

    #[test]
    fn debug_session_create_rejects_ttd_kind_with_file_hint() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "debug.session.create".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": { "kind": "ttd", "path": "trace.run" }
            })),
        });

        let error = response.error.unwrap();
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("kind `ttd`"));
        assert!(error.message.contains("kind\": \"file"));
    }

    #[test]
    fn debug_session_create_accepts_active_interactive_user_override() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(RecordingIdentitySupervisor {
            identities: Mutex::new(Vec::new()),
        });
        let host = ServiceHost::new(supervisor.clone());
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "debug.session.create".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "worker_identity": "active_interactive_user",
                "target": { "kind": "file", "path": "sample.dmp" }
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(
            supervisor.identities.lock().unwrap().as_slice(),
            &[Some(WorkerIdentity::ActiveInteractiveUser)]
        );
    }

    #[test]
    fn debug_session_create_launch_defaults_missing_args_to_empty() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "debug.session.create".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": { "kind": "launch", "executable": "cmd.exe" }
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(response.result.unwrap()["operation_status"], "success");
    }

    #[test]
    fn debug_session_create_access_denied_suggests_active_user_override() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::new(Arc::new(AccessDeniedStartSupervisor));
        let response = create_debug_session(&host, temp.path());

        let error = response.error.unwrap();
        assert!(
            error
                .message
                .contains("recommended_retry_worker_identity=active_interactive_user")
        );
        assert!(error.message.contains("worker_identity"));
        assert!(error.message.contains("active_interactive_user"));
    }

    #[test]
    fn file_session_create_auto_retries_active_user_on_access_denied() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(AccessDeniedThenActiveSupervisor {
            identities: Mutex::new(Vec::new()),
        });
        let host = ServiceHost::new(supervisor.clone()).with_capabilities(ServiceCapabilities {
            dbgeng_dirs: vec![PathBuf::from(r"C:\DbgEng")],
            ..Default::default()
        });
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "debug.session.create".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": { "kind": "file", "path": "sample.dmp" }
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(
            supervisor.identities.lock().unwrap().as_slice(),
            &[None, Some(WorkerIdentity::ActiveInteractiveUser)]
        );
    }

    #[test]
    fn failed_session_create_is_recorded_as_failed_operation() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::new(Arc::new(FailingStartSupervisor));
        let response = create_debug_session(&host, temp.path());

        assert!(response.error.is_some());
        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let operations = workspace.list_operations().unwrap();
        let operation = operations
            .iter()
            .find(|operation| operation.capability == "debug.session.create")
            .unwrap();
        assert!(matches!(operation.status, OperationStatus::Failed));
        assert!(operation.summary.contains("start_failed"));
    }

    #[test]
    fn session_id_is_enough_after_create() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let create = create_debug_session(&host, temp.path());
        let session_id = create.result.unwrap()["session_id"].clone();

        let eval = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "command": ".echo hello"
            })),
        });

        assert!(eval.error.is_none(), "{:?}", eval.error);
        assert!(
            eval.result.unwrap()["output"]
                .as_str()
                .unwrap()
                .contains(".echo hello")
        );
    }

    #[test]
    fn debug_eval_truncates_inline_output_and_keeps_raw_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let eval = eval_request(&host, session_id.clone(), ".mock_long_output");

        assert!(eval.error.is_none(), "{:?}", eval.error);
        let result = eval.result.unwrap();
        assert_eq!(result["output_truncated"], true);
        assert_eq!(
            result["output"].as_str().unwrap().len(),
            DEFAULT_INLINE_TEXT_BYTE_LIMIT
        );
        assert_eq!(
            result["full_output_byte_len"],
            json!(DEFAULT_INLINE_TEXT_BYTE_LIMIT + 100)
        );
        assert_eq!(result["full_output_artifact_ref"], result["raw_output_ref"]);

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let raw_ref: ArtifactRef =
            serde_json::from_value(result["raw_output_ref"].clone()).unwrap();
        let raw = workspace.get_artifact(&raw_ref).unwrap().unwrap();
        let raw_path = workspace
            .resolve_artifact_relative_path(raw.relative_path)
            .unwrap();
        assert_eq!(
            fs::read_to_string(raw_path).unwrap().len(),
            DEFAULT_INLINE_TEXT_BYTE_LIMIT + 100
        );

        let close = close_request(&host, session_id);
        assert!(close.error.is_none(), "{:?}", close.error);
    }

    #[test]
    fn debug_eval_steps_runs_commands_in_order() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval_steps".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "commands": [
                    ".exepath D:\\Repos\\Sangfor\\EDR\\sf3\\dump",
                    ".reload /f sf3.dll",
                    "!chkimg -lo 100 -d sf3"
                ]
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["operation_status"], "success");
        assert_eq!(result["steps"].as_array().unwrap().len(), 3);
        assert!(
            result["steps"][0]["output"]
                .as_str()
                .unwrap()
                .contains(".exepath")
        );

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let operations = workspace.list_operations().unwrap();
        assert!(
            operations
                .iter()
                .any(|operation| operation.capability == "debug.eval_steps")
        );
        assert_eq!(
            operations
                .iter()
                .filter(|operation| operation.capability == "debug.eval_steps.step")
                .count(),
            3
        );
    }

    #[test]
    fn debug_eval_steps_reports_failed_step_without_running_later_steps() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::new(Arc::new(FailingEvalSupervisor));
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval_steps".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "commands": [".echo first", ".echo second"]
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["operation_status"], "failed");
        assert_eq!(result["failed_step_index"], 0);
        assert_eq!(result["steps"].as_array().unwrap().len(), 1);
        assert!(
            result["steps"][0]["error"]
                .as_str()
                .unwrap()
                .contains("mock eval failed")
        );
    }

    #[test]
    fn debug_eval_steps_propagates_internal_step_errors() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::new(Arc::new(BadEvalWriteSupervisor));
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval_steps".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "commands": [".echo first"]
            })),
        });

        let error = response.error.unwrap();
        assert!(error.message.contains("artifact path"));
        let state = host.lock_state().unwrap();
        assert!(state.operations.values().any(|operation| {
            operation.capability == "debug.eval_steps.step"
                && operation.status == ServiceOperationStatus::Failed
        }));
        assert!(state.operations.values().any(|operation| {
            operation.capability == "debug.eval_steps"
                && operation.status == ServiceOperationStatus::Failed
        }));
    }

    #[test]
    fn service_info_does_not_expose_worker_details() {
        let host = ServiceHost::with_mock_workers();
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "service.info".to_string(),
            params: None,
        });

        assert!(response.error.is_none());
        let result = response.result.unwrap();
        assert!(result.get("worker_binding").is_none());
        assert!(result.get("worker_identity").is_none());
    }

    #[test]
    fn process_worker_identity_policy_separates_dev_and_installed_service() {
        let dev = ProcessWorkerSupervisor::new().unwrap();
        assert_eq!(dev.identity, WorkerIdentity::CurrentUserDevMode);

        let installed = ProcessWorkerSupervisor::new_installed_service().unwrap();
        assert_eq!(installed.identity, WorkerIdentity::LocalSystem);
    }

    #[cfg(windows)]
    #[test]
    fn active_user_worker_caches_windowsapps_dbgeng_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp
            .path()
            .join("WindowsApps")
            .join("Microsoft.WinDbg_1.0.0.0_x64__8wekyb3d8bbwe")
            .join("amd64");
        fs::create_dir_all(source.join("ttd")).unwrap();
        fs::write(source.join("dbgeng.dll"), b"dbgeng").unwrap();
        fs::write(source.join("ttd").join("TTDReplay.dll"), b"ttd").unwrap();
        let cache_root = temp.path().join("cache");

        let cached = cache_windowsapps_dbgeng_dir_for_active_user(&source, &cache_root).unwrap();
        assert_ne!(cached, source);
        assert!(cached.join("dbgeng.dll").is_file());
        assert!(cached.join("ttd").join("TTDReplay.dll").is_file());

        let active =
            prepare_active_user_dbgeng_dirs(std::slice::from_ref(&source), &cache_root).unwrap();
        assert_ne!(active[0], source);

        let system = prepare_dbgeng_dirs_for_worker_identity(
            &WorkerIdentity::LocalSystem,
            std::slice::from_ref(&source),
        )
        .unwrap();
        assert_eq!(system[0], source);
    }

    #[cfg(windows)]
    #[test]
    fn windbg_runtime_staging_activates_dbgeng_and_ttd_together() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        let source = temp
            .path()
            .join("WindowsApps")
            .join("Microsoft.WinDbg_1.0.0.0_x64__8wekyb3d8bbwe")
            .join("amd64");
        write_complete_windbg_runtime(&source);
        let staging = paths
            .rt_dir
            .join("windbg")
            .join(format!("{}.next-test", windbg_runtime_arch()));

        let staged =
            stage_windbg_runtime_from_source(&source, &paths.windbg_runtime_dir, &staging).unwrap();
        assert!(staged.staging_dir.join("dbgeng.dll").is_file());
        assert!(
            staged
                .staging_dir
                .join("ttd")
                .join("TTDRecordCPU.dll")
                .is_file()
        );

        activate_staged_windbg_runtime(&staged, "test").unwrap();

        assert!(paths.windbg_runtime_dir.join("dbgeng.dll").is_file());
        assert!(
            paths
                .windbg_runtime_dir
                .join("ttd")
                .join("TTD.exe")
                .is_file()
        );
        assert!(!staging.exists());
    }

    #[cfg(windows)]
    #[test]
    fn service_update_stages_existing_windbg_runtime_inside_next_bin() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        write_complete_windbg_runtime(&paths.windbg_runtime_dir);
        let staging_bin = paths.root_dir.join("bin.next-test");
        fs::create_dir_all(&staging_bin).unwrap();

        let staged =
            prepare_update_windbg_runtime_in_staging(&paths, &staging_bin, "test").unwrap();

        let staged = staged.expect("existing runtime should be staged");
        assert_eq!(
            staged,
            staging_bin
                .join(WINDOWS_SERVICE_RT_DIR)
                .join("windbg")
                .join(windbg_runtime_arch())
        );
        assert!(staged.join("dbgeng.dll").is_file());
        assert!(staged.join("ttd").join("TTD.exe").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn windbg_runtime_preserve_candidates_include_legacy_root_rt() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
        write_complete_windbg_runtime(&paths.legacy_windbg_runtime_dir());
        let destination = paths
            .root_dir
            .join("bin.next-test")
            .join(WINDOWS_SERVICE_RT_DIR)
            .join("windbg")
            .join(windbg_runtime_arch());

        let copied_from = copy_existing_windbg_runtime_to_destination(
            &windbg_runtime_source_candidates(&paths),
            &destination,
        )
        .unwrap();

        assert_eq!(copied_from, Some(paths.legacy_windbg_runtime_dir()));
        assert!(destination.join("dbgeng.dll").is_file());
        assert!(destination.join("ttd").join("TTD.exe").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn windbg_runtime_validation_requires_ttd_recorder_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("amd64");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("dbgeng.dll"), b"dbgeng").unwrap();

        let error = validate_windbg_runtime_dir(&source).unwrap_err();

        assert!(error.to_string().contains("TTD.exe"));
        assert!(error.to_string().contains("TTDRecordCPU.dll"));
    }

    #[cfg(windows)]
    fn write_complete_windbg_runtime(root: &Path) {
        fs::create_dir_all(root.join("ttd")).unwrap();
        let required: &[&[&str]] = &[
            &["dbgeng.dll"],
            &["dbghelp.dll"],
            &["dbgmodel.dll"],
            &["ttd", "TTD.exe"],
            &["ttd", "TTDInject.exe"],
            &["ttd", "TTDLoader.dll"],
            &["ttd", "TTDRecord.dll"],
            &["ttd", "TTDRecordCPU.dll"],
            &["ttd", "TTDReplay.dll"],
            &["ttd", "TTDReplayCPU.dll"],
        ];
        for relative in required {
            let path = relative
                .iter()
                .fold(root.to_path_buf(), |path, part| path.join(part));
            fs::write(path, b"runtime").unwrap();
        }
    }

    #[test]
    fn mcp_tool_schemas_expose_structured_targets_and_write_items() {
        let tools = mcp_tool_descriptors(ServiceCapabilities::default());
        let debug = tools
            .iter()
            .find(|tool| tool["name"] == "debug.session.create")
            .unwrap();
        assert!(debug["description"].as_str().unwrap().contains("kind=file"));
        assert!(debug["inputSchema"]["properties"]["target"]["oneOf"].is_array());
        assert_eq!(
            debug["inputSchema"]["properties"]["target"]["oneOf"][2]["required"],
            json!(["kind", "executable"])
        );
        assert_eq!(
            debug["inputSchema"]["properties"]["worker_identity"]["enum"][1],
            "active_interactive_user"
        );
        let eval = tools
            .iter()
            .find(|tool| tool["name"] == "debug.eval")
            .unwrap();
        assert!(
            eval["description"]
                .as_str()
                .unwrap()
                .contains("IDebugControl::Execute")
        );
        assert!(
            eval["description"]
                .as_str()
                .unwrap()
                .contains("does not emulate WinDbg command-window multiline input")
        );
        assert!(
            eval["inputSchema"]["properties"]["command"]["description"]
                .as_str()
                .unwrap()
                .contains("single IDebugControl::Execute call")
        );
        assert_eq!(
            eval["inputSchema"]["properties"]["timeout_ms"]["type"],
            "integer"
        );
        let eval_steps = tools
            .iter()
            .find(|tool| tool["name"] == "debug.eval_steps")
            .unwrap();
        assert!(
            eval_steps["description"]
                .as_str()
                .unwrap()
                .contains("separate IDebugControl::Execute call")
        );
        assert!(
            eval_steps["inputSchema"]["properties"]["commands"]["description"]
                .as_str()
                .unwrap()
                .contains("Each item is executed separately")
        );
        assert_eq!(
            eval_steps["inputSchema"]["properties"]["commands"]["items"]["type"],
            "string"
        );

        let recording = tools
            .iter()
            .find(|tool| tool["name"] == "recording.ttd")
            .unwrap();
        assert!(
            recording["description"]
                .as_str()
                .unwrap()
                .contains("register the .run/.idx artifacts")
        );
        assert_eq!(
            recording["inputSchema"]["properties"]["target"]["oneOf"][0]["required"],
            json!(["kind", "executable"])
        );
        assert_eq!(
            recording["inputSchema"]["properties"]["target"]["oneOf"][2]["required"],
            json!(["kind", "program"])
        );
        assert_eq!(
            recording["inputSchema"]["properties"]["target"]["oneOf"][2]["properties"]["cmd_line_filter"]
                ["type"],
            "string"
        );
        assert_eq!(
            recording["inputSchema"]["properties"]["worker_identity"]["enum"][1],
            "active_interactive_user"
        );
        assert_eq!(
            recording["inputSchema"]["properties"]["options"]["properties"]["record_mode"]["enum"],
            json!(["automatic", "manual"])
        );
        assert_eq!(
            recording["inputSchema"]["properties"]["options"]["properties"]["replay_cpu_support"]["default"],
            "default"
        );

        let add_symbols = tools
            .iter()
            .find(|tool| tool["name"] == "debug.add_symbols")
            .unwrap();
        assert!(
            add_symbols["description"]
                .as_str()
                .unwrap()
                .contains(".sympath+")
        );
        assert!(
            add_symbols["inputSchema"]["properties"]["symbol_path"]["description"]
                .as_str()
                .unwrap()
                .contains("does not replace")
        );

        let open = tools
            .iter()
            .find(|tool| tool["name"] == "reverse.session.open")
            .unwrap();
        assert!(
            open["description"]
                .as_str()
                .unwrap()
                .contains("not a read-only clone")
        );

        let decompile = tools
            .iter()
            .find(|tool| tool["name"] == "reverse.decompile")
            .unwrap();
        let session_schema = &decompile["inputSchema"]["properties"]["session_id"];
        assert_eq!(session_schema["oneOf"][0]["type"], "string");
        assert_eq!(
            session_schema["oneOf"][1]["properties"]["id"]["type"],
            "string"
        );
        assert!(
            session_schema["description"]
                .as_str()
                .unwrap()
                .contains("\"session-")
        );

        let inspect_item = tools
            .iter()
            .find(|tool| tool["name"] == "reverse.inspect_item")
            .unwrap();
        assert!(
            inspect_item["description"]
                .as_str()
                .unwrap()
                .contains("without mutating")
        );

        let rename = tools
            .iter()
            .find(|tool| tool["name"] == "reverse.rename")
            .unwrap();
        assert!(
            rename["description"]
                .as_str()
                .unwrap()
                .contains("call reverse.idb_save")
        );
        assert!(
            rename["inputSchema"]["properties"]["items"]["items"]["description"]
                .as_str()
                .unwrap()
                .contains("addr or name")
        );
        assert_eq!(
            rename["inputSchema"]["properties"]["items"]["items"]["properties"]["new_name"]["type"],
            "string"
        );

        let set_type = tools
            .iter()
            .find(|tool| tool["name"] == "reverse.set_type")
            .unwrap();
        assert!(set_type["inputSchema"]["properties"]["items"]["items"]["properties"]["kind"]
            ["description"]
            .as_str()
            .unwrap()
            .contains("function declaration"));

        let idb_save = tools
            .iter()
            .find(|tool| tool["name"] == "reverse.idb_save")
            .unwrap();
        assert!(
            idb_save["description"]
                .as_str()
                .unwrap()
                .contains("save in place")
        );

        let close = tools
            .iter()
            .find(|tool| tool["name"] == "debug.session.close")
            .unwrap();
        assert!(
            close["description"]
                .as_str()
                .unwrap()
                .contains("Cooperatively close")
        );
        let kill = tools
            .iter()
            .find(|tool| tool["name"] == "debug.session.kill")
            .unwrap();
        assert!(
            kill["description"]
                .as_str()
                .unwrap()
                .contains("Forcefully terminate")
        );
    }

    #[test]
    fn repeated_close_is_rejected_after_session_is_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let first = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.session.close".to_string(),
            params: Some(json!({ "session_id": session_id.clone() })),
        });
        assert!(first.error.is_none(), "{:?}", first.error);

        let second = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "debug.session.close".to_string(),
            params: Some(json!({ "session_id": session_id })),
        });
        assert_eq!(second.error.unwrap().code, -32013);
    }

    #[test]
    fn eval_registers_raw_output_and_transcript_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let eval = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "command": ".echo artifacts"
            })),
        });
        assert!(eval.error.is_none(), "{:?}", eval.error);
        let result = eval.result.as_ref().unwrap();
        assert_eq!(result["operation_status"], "success");
        assert_eq!(result["artifact_refs"].as_array().unwrap().len(), 3);
        assert!(result["raw_output_ref"].get("id").is_some());

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "debug.raw_output")
        );
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "debug.transcript")
        );
        let operations = workspace.list_operations().unwrap();
        let eval_operation = operations
            .iter()
            .find(|operation| operation.capability == "debug.eval")
            .unwrap();
        assert_eq!(eval_operation.artifacts.len(), 3);
        assert!(eval_operation.raw_output.is_some());
        assert_eq!(workspace.list_command_audit().unwrap().len(), 1);
    }

    #[test]
    fn failed_eval_is_recorded_in_command_audit() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::new(Arc::new(FailingEvalSupervisor));
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let eval = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "command": ".echo fails"
            })),
        });
        assert!(eval.error.is_some());

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let audit = workspace.list_command_audit().unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].command, ".echo fails");
        assert!(matches!(audit[0].status, OperationStatus::Failed));
    }

    #[test]
    fn debug_eval_timeout_kills_worker_and_marks_session_error() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(200));
        let host = ServiceHost::new(supervisor);
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let eval = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "command": "TTD.Calls(\"KERNELBASE!LoadLibraryExW\")",
                "timeout_ms": 10
            })),
        });

        let error = eval.error.unwrap();
        assert!(error.message.contains("timed out after 10 ms"));

        let state = host.lock_state().unwrap();
        let session = state.sessions.values().next().unwrap();
        assert_eq!(session.state, DebugSessionState::Error);
        let operation = state
            .operations
            .values()
            .find(|operation| operation.capability == "debug.eval")
            .unwrap();
        assert_eq!(operation.status, ServiceOperationStatus::Failed);
    }

    #[test]
    fn add_symbols_uses_distinct_operation_capability() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.add_symbols".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "symbol_path": r"srv*C:\symbols*https://msdl.microsoft.com/download/symbols",
                "reload": true
            })),
        });
        assert!(response.error.is_none(), "{:?}", response.error);

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let operations = workspace.list_operations().unwrap();
        assert!(
            operations
                .iter()
                .any(|operation| operation.capability == "debug.add_symbols")
        );
    }

    #[test]
    fn cancel_rejects_completed_operation_without_changing_status() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
        let eval = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "command": ".echo done"
            })),
        });
        let operation_id = eval.result.unwrap()["operation_id"].clone();

        let cancel = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "operation.cancel".to_string(),
            params: Some(json!({ "operation_id": operation_id.clone() })),
        });
        assert_eq!(cancel.error.unwrap().code, -32014);

        let get = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(4)),
            method: "operation.get".to_string(),
            params: Some(json!({ "operation_id": operation_id })),
        });
        assert_eq!(get.result.unwrap()["status"], "success");
    }

    #[test]
    fn cancel_marks_running_operation_as_canceled() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(InstrumentedWorkerSupervisor::wait_for_cancel());
        let host = ServiceHost::new(supervisor.clone());
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
        let eval_host = host.clone();
        let eval_session_id = session_id.clone();

        let eval_thread = std::thread::spawn(move || {
            eval_host.handle_rpc(JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(json!(2)),
                method: "debug.eval".to_string(),
                params: Some(json!({
                    "session_id": eval_session_id,
                    "command": ".echo wait"
                })),
            })
        });

        let operation_id = supervisor.wait_for_operation_id();
        let cancel = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "operation.cancel".to_string(),
            params: Some(json!({ "operation_id": operation_id.clone() })),
        });
        assert!(cancel.error.is_none(), "{:?}", cancel.error);
        let eval = eval_thread.join().unwrap();
        assert!(eval.error.is_none(), "{:?}", eval.error);

        let get = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(4)),
            method: "operation.get".to_string(),
            params: Some(json!({ "operation_id": operation_id })),
        });
        assert_eq!(get.result.unwrap()["status"], "canceled");
    }

    #[test]
    fn eval_requests_for_one_session_are_serialized() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(100));
        let host = ServiceHost::new(supervisor.clone());
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
        let first_host = host.clone();
        let second_host = host.clone();
        let first_session = session_id.clone();
        let second_session = session_id;

        let first = std::thread::spawn(move || eval_request(&first_host, first_session, ".echo 1"));
        let second =
            std::thread::spawn(move || eval_request(&second_host, second_session, ".echo 2"));
        assert!(first.join().unwrap().error.is_none());
        assert!(second.join().unwrap().error.is_none());

        assert_eq!(supervisor.max_active.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn eval_requests_for_different_sessions_can_run_concurrently() {
        let temp_a = tempfile::tempdir().unwrap();
        let temp_b = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(150));
        let host = ServiceHost::new(supervisor.clone());
        let session_a =
            create_debug_session(&host, temp_a.path()).result.unwrap()["session_id"].clone();
        let session_b =
            create_debug_session(&host, temp_b.path()).result.unwrap()["session_id"].clone();
        let first_host = host.clone();
        let second_host = host.clone();

        let first = std::thread::spawn(move || eval_request(&first_host, session_a, ".echo a"));
        let second = std::thread::spawn(move || eval_request(&second_host, session_b, ".echo b"));
        assert!(first.join().unwrap().error.is_none());
        assert!(second.join().unwrap().error.is_none());

        assert!(supervisor.max_active.load(Ordering::SeqCst) > 1);
    }

    #[test]
    fn kill_does_not_wait_for_running_session_request() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(InstrumentedWorkerSupervisor::wait_for_cancel());
        let host = ServiceHost::new(supervisor.clone());
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
        let eval_host = host.clone();
        let eval_session_id = session_id.clone();
        let eval_thread = std::thread::spawn(move || {
            eval_request(&eval_host, eval_session_id, ".echo hang-until-kill")
        });
        let _operation_id = supervisor.wait_for_operation_id();

        let kill = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "debug.session.kill".to_string(),
            params: Some(json!({ "session_id": session_id })),
        });
        assert!(kill.error.is_none(), "{:?}", kill.error);
        assert_eq!(kill.result.unwrap()["state"], "Closed");
        let _ = eval_thread.join().unwrap();
    }

    #[test]
    fn concurrent_close_rechecks_latest_session_state() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(100));
        let host = ServiceHost::new(supervisor);
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
        let first_host = host.clone();
        let second_host = host.clone();
        let first_session = session_id.clone();
        let second_session = session_id;

        let first = std::thread::spawn(move || close_request(&first_host, first_session));
        let second = std::thread::spawn(move || close_request(&second_host, second_session));
        let responses = vec![first.join().unwrap(), second.join().unwrap()];
        let success_count = responses
            .iter()
            .filter(|response| response.error.is_none())
            .count();
        let terminal_count = responses
            .iter()
            .filter(|response| {
                response
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == -32013)
            })
            .count();

        assert_eq!(success_count, 1);
        assert_eq!(terminal_count, 1);
    }

    #[test]
    fn eval_after_concurrent_close_rechecks_latest_session_state() {
        let temp = tempfile::tempdir().unwrap();
        let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(100));
        let host = ServiceHost::new(supervisor);
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
        let close_host = host.clone();
        let eval_host = host.clone();
        let close_session = session_id.clone();
        let eval_session = session_id;

        let close = std::thread::spawn(move || close_request(&close_host, close_session));
        std::thread::sleep(Duration::from_millis(10));
        let eval = std::thread::spawn(move || eval_request(&eval_host, eval_session, ".echo late"));
        assert!(close.join().unwrap().error.is_none());
        assert_eq!(eval.join().unwrap().error.unwrap().code, -32012);
    }

    #[test]
    fn read_memory_registers_memory_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.read_memory".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "address": 4096,
                "length": 16
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["bytes_read"], 16);
        assert_eq!(result["operation_status"], "success");
        assert!(result["memory_ref"].get("id").is_some());

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "debug.memory")
        );
        let operations = workspace.list_operations().unwrap();
        assert!(
            operations
                .iter()
                .any(|operation| operation.capability == "debug.read_memory")
        );
    }

    #[test]
    fn reverse_lookup_records_session_lookup_and_workspace_facts() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.exe");
        fs::write(&database, b"sample").unwrap();
        let host = ServiceHost::with_mock_workers();

        let open = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "reverse.session.open".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "database_path": database,
                "ida_install_dir": temp.path().join("ida")
            })),
        });
        assert!(open.error.is_none(), "{:?}", open.error);
        let open_result = open.result.unwrap();
        let session_id = open_result["session_id"].clone();

        let lookup = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "reverse.lookup_function".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "runtime_address": "0x180001234",
                "runtime_module_base": "0x180000000",
                "ida_image_base": "0x140000000"
            })),
        });
        assert!(lookup.error.is_none(), "{:?}", lookup.error);
        let lookup_result = lookup.result.unwrap();
        assert_eq!(lookup_result["rva"], json!(0x1234));
        assert_eq!(lookup_result["ida_ea"], json!(0x140001234u64));
        assert_eq!(lookup_result["found"], json!(true));

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "reverse.session")
        );
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "reverse.lookup")
        );
        let operations = workspace.list_operations().unwrap();
        assert!(
            operations
                .iter()
                .any(|operation| operation.capability == "reverse.session.open")
        );
        assert!(
            operations
                .iter()
                .any(|operation| operation.capability == "reverse.lookup_function")
        );
        let session_id = serde_json::from_value::<SessionRef>(session_id).unwrap();
        assert!(
            workspace
                .root()
                .join("artifacts")
                .join("reverse_sessions")
                .join(session_id.id.as_str())
                .join("sessions")
                .is_dir()
        );
        assert!(
            workspace
                .root()
                .join("artifacts")
                .join("reverse_sessions")
                .join(session_id.id.as_str())
                .join("lookups")
                .is_dir()
        );
        assert!(
            workspace
                .root()
                .join("artifacts")
                .join("reverse_sessions")
                .join(session_id.id.as_str())
                .join("sessions")
                .read_dir()
                .unwrap()
                .next()
                .is_some()
        );
        assert!(
            workspace
                .root()
                .join("artifacts")
                .join("reverse_sessions")
                .join(session_id.id.as_str())
                .join("lookups")
                .read_dir()
                .unwrap()
                .next()
                .is_some()
        );
    }

    #[test]
    fn reverse_core_functions_record_artifacts_and_operations() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers().with_ida_py_eval(true);
        let session_id = open_reverse_session(&host, temp.path());
        let calls = [
            (
                "reverse.lookup_funcs",
                json!({
                    "queries": ["0x140001234", "main"],
                    "runtime_module_base": 0,
                    "ida_image_base": 0
                }),
            ),
            (
                "reverse.int_convert",
                json!({ "inputs": "42, 0x2a, ascii:AB" }),
            ),
            (
                "reverse.list_funcs",
                json!({ "offset": 0, "count": 2, "filter": "parse" }),
            ),
            ("reverse.list_globals", json!({ "offset": 0, "count": 10 })),
            ("reverse.list_imports", json!({ "offset": 1, "count": 1 })),
            (
                "reverse.list_strings",
                json!({ "offset": 0, "count": 2, "filter": "config" }),
            ),
            ("reverse.get_string", json!({ "addr": "0x140040000" })),
            (
                "reverse.get_bytes",
                json!({ "addr": "0x140040000", "length": 8 }),
            ),
            (
                "reverse.get_int",
                json!({ "addr": "0x140040000", "size": 4, "endian": "little" }),
            ),
            ("reverse.decompile", json!({ "addr": "0x140001234" })),
            ("reverse.disasm", json!({ "addr": "0x140001234" })),
            (
                "reverse.xrefs_to",
                json!({ "addrs": ["0x140001234", "0x140001300"] }),
            ),
            (
                "reverse.xrefs_to_field",
                json!({ "queries": "MY_STRUCT.field_0" }),
            ),
            ("reverse.callees", json!({ "addrs": ["0x140001000"] })),
            (
                "reverse.rename",
                json!({ "items": [{ "kind": "function", "addr": "0x140001000", "new_name": "dbgatlas_main" }] }),
            ),
            (
                "reverse.set_comments",
                json!({ "items": [{ "addr": "0x140001000", "text": "entry point" }] }),
            ),
            (
                "reverse.set_type",
                json!({ "items": [{ "kind": "function", "addr": "0x140001000", "type": "int dbgatlas_main(void)" }] }),
            ),
            (
                "reverse.declare_type",
                json!({ "decls": "struct DbgAtlasContext { int state; };" }),
            ),
            (
                "reverse.inspect_item",
                json!({ "queries": ["0x140001000"] }),
            ),
            (
                "reverse.force_recompile",
                json!({ "addrs": ["0x140001000"] }),
            ),
            ("reverse.idb_save", json!({})),
            (
                "reverse.find_bytes",
                json!({ "patterns": ["48 8B ?? ??"], "limit": 2 }),
            ),
            (
                "reverse.search_text",
                json!({ "query": "config", "scope": "all", "limit": 2 }),
            ),
            (
                "reverse.py_eval",
                json!({ "code": "print('hello from dbgatlas')" }),
            ),
            (
                "reverse.query_xrefs",
                json!({ "target": "0x140001000", "direction": "to", "xref_type": "all" }),
            ),
            (
                "reverse.query_funcs",
                json!({ "filter": "parse", "sort_by": "name" }),
            ),
            (
                "reverse.query_entities",
                json!({ "kind": "functions", "filter": "main" }),
            ),
        ];

        for (method, mut args) in calls {
            args["session_id"] = session_id.clone();
            let response = host.handle_rpc(JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(json!(3)),
                method: method.to_string(),
                params: Some(args),
            });
            assert!(response.error.is_none(), "{method}: {:?}", response.error);
            let result = response.result.unwrap();
            assert_eq!(result["operation_status"], "success");
            assert!(result["artifact_refs"].as_array().unwrap().len() == 1);
            assert!(result.get("result").is_some());
            assert_eq!(
                result["function"],
                method
                    .strip_prefix("reverse.")
                    .expect("reverse method prefix is present")
            );
        }

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .filter(|artifact| artifact.kind == "reverse.core")
                .count()
                >= 25
        );
        let operations = workspace.list_operations().unwrap();
        for capability in [
            "reverse.lookup_funcs",
            "reverse.int_convert",
            "reverse.list_funcs",
            "reverse.list_globals",
            "reverse.list_imports",
            "reverse.list_strings",
            "reverse.get_string",
            "reverse.get_bytes",
            "reverse.get_int",
            "reverse.decompile",
            "reverse.disasm",
            "reverse.xrefs_to",
            "reverse.xrefs_to_field",
            "reverse.callees",
            "reverse.rename",
            "reverse.set_comments",
            "reverse.set_type",
            "reverse.declare_type",
            "reverse.inspect_item",
            "reverse.force_recompile",
            "reverse.idb_save",
            "reverse.find_bytes",
            "reverse.search_text",
            "reverse.py_eval",
            "reverse.query_xrefs",
            "reverse.query_funcs",
            "reverse.query_entities",
        ] {
            assert!(
                operations
                    .iter()
                    .any(|operation| operation.capability == capability),
                "{capability} was not recorded"
            );
        }
    }

    #[test]
    fn reverse_session_metadata_describes_write_capability() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let _session_id = open_reverse_session(&host, temp.path());

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        let session_artifact = artifacts
            .iter()
            .find(|artifact| artifact.kind == "reverse.session")
            .unwrap();
        let metadata_path = workspace.root().join(&session_artifact.relative_path);
        let metadata: Value =
            serde_json::from_str(&fs::read_to_string(metadata_path).unwrap()).unwrap();
        assert_eq!(metadata["writes_idb"], false);
        assert_eq!(metadata["open_operation_writes_idb"], false);
        assert_eq!(metadata["session_write_capable"], true);
    }

    #[test]
    fn reverse_core_accepts_string_session_id() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id = open_reverse_session(&host, temp.path());
        let session_id_text = session_id["id"].as_str().unwrap().to_string();

        let response = reverse_core_rpc(
            &host,
            "reverse.list_funcs",
            json!(session_id_text),
            json!({ "offset": 0, "count": 1 }),
        );

        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(response.result.unwrap()["function"], "list_funcs");
    }

    #[test]
    fn reverse_write_results_include_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id = open_reverse_session(&host, temp.path());

        let set_type = reverse_core_rpc(
            &host,
            "reverse.set_type",
            session_id.clone(),
            json!({
                "items": [{
                    "kind": "global",
                    "addr": "0x140020000",
                    "type": "struct Descriptor *[18]"
                }]
            }),
        );
        assert!(set_type.error.is_none(), "{:?}", set_type.error);
        let set_type = set_type.result.unwrap();
        let set_item = &set_type["result"]["items"][0];
        assert_eq!(set_item["ok"], true);
        assert!(
            set_item["generated_decl"]
                .as_str()
                .unwrap()
                .contains("mock_symbol")
        );
        assert_eq!(set_item["item_state"]["item_head"], json!(0x140020000u64));

        let declare = reverse_core_rpc(
            &host,
            "reverse.declare_type",
            session_id,
            json!({
                "decls": [
                    "struct GoodType { int x; };",
                    "invalid declaration"
                ]
            }),
        );
        assert!(declare.error.is_none(), "{:?}", declare.error);
        let declare = declare.result.unwrap();
        assert_eq!(declare["result"]["ok"], false);
        assert_eq!(declare["result"]["changed_count"], 1);
        assert_eq!(declare["result"]["errors"], 1);
        assert_eq!(declare["result"]["items"][0]["ok"], true);
        assert_eq!(declare["result"]["items"][1]["ok"], false);
        assert!(
            declare["result"]["items"][1]["hint"]
                .as_str()
                .unwrap()
                .contains("failed to parse")
        );
    }

    #[test]
    fn reverse_core_legacy_names_are_not_accepted() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id = open_reverse_session(&host, temp.path());

        for (method, args) in [
            ("reverse.imports", json!({ "offset": 0, "count": 1 })),
            ("reverse.xref_query", json!({ "target": "0x140001000" })),
            ("reverse.func_query", json!({ "filter": "parse" })),
            ("reverse.entity_query", json!({ "kind": "functions" })),
        ] {
            let response = reverse_core_rpc(&host, method, session_id.clone(), args);
            let error = response.error.expect("legacy method is rejected");
            assert!(
                error.message.contains("unknown method"),
                "{method}: {:?}",
                error
            );
        }
    }

    #[test]
    fn reverse_py_eval_is_disabled_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id = open_reverse_session(&host, temp.path());
        let response = reverse_core_rpc(
            &host,
            "reverse.py_eval",
            session_id,
            json!({ "code": "print('should not run')" }),
        );

        let error = response.error.expect("reverse.py_eval is rejected");
        assert!(error.message.contains("disabled by runtime policy"));
    }

    #[test]
    fn reverse_decompile_large_result_returns_preview_with_artifact_ref() {
        let artifact_id = ArtifactRef::new(Id::new("artifact-large-decompile").unwrap());
        let core = ReverseCoreFunctionResult {
            function: "decompile".to_string(),
            result: json!({
                "addr": 0x140001000u64,
                "function_name": "big_function",
                "language": "c",
                "pseudocode": "x".repeat(MAX_INLINE_REVERSE_RESULT_BYTES + 1),
            }),
            warnings: Vec::new(),
        };

        let result = reverse_core_response_result(&core, &artifact_id, 200_000).unwrap();

        assert_eq!(result["pseudocode_truncated"], true);
        assert_eq!(
            result["pseudocode"].as_str().unwrap().len(),
            MAX_INLINE_DECOMPILE_PSEUDOCODE_BYTES
        );
        assert_eq!(
            result["full_result_artifact_ref"]["id"],
            "artifact-large-decompile"
        );
        assert_eq!(result["full_result_byte_len"], 200_000);
    }

    #[test]
    fn reverse_core_functions_cover_pagination_empty_and_invalid_input() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id = open_reverse_session(&host, temp.path());

        let page = reverse_core_rpc(
            &host,
            "reverse.list_funcs",
            session_id.clone(),
            json!({ "offset": 1, "count": 2 }),
        );
        assert!(page.error.is_none(), "{:?}", page.error);
        let result = page.result.unwrap();
        assert_eq!(result["result"]["offset"], 1);
        assert_eq!(result["result"]["count"], 2);
        assert_eq!(result["result"]["total"], 4);

        let empty = reverse_core_rpc(
            &host,
            "reverse.list_funcs",
            session_id.clone(),
            json!({ "filter": "does-not-exist" }),
        );
        assert!(empty.error.is_none(), "{:?}", empty.error);
        assert_eq!(empty.result.unwrap()["result"]["total"], 0);

        let empty_strings = reverse_core_rpc(
            &host,
            "reverse.list_strings",
            session_id.clone(),
            json!({ "offset": 10, "count": 2 }),
        );
        assert!(empty_strings.error.is_none(), "{:?}", empty_strings.error);
        let result = empty_strings.result.unwrap();
        assert_eq!(result["result"]["offset"], 10);
        assert_eq!(result["result"]["count"], 0);
        assert_eq!(result["result"]["total"], 3);

        for (method, args) in [
            (
                "reverse.get_bytes",
                json!({ "addr": "0x140040000", "length": 0 }),
            ),
            (
                "reverse.get_bytes",
                json!({ "addr": "0x140040000", "length": 4097 }),
            ),
            (
                "reverse.get_int",
                json!({ "addr": "0x140040000", "size": 3 }),
            ),
        ] {
            let invalid = reverse_core_rpc(&host, method, session_id.clone(), args);
            assert!(invalid.error.is_some(), "{method} accepted invalid input");
        }

        let invalid = reverse_core_rpc(
            &host,
            "reverse.decompile",
            session_id,
            json!({ "addr": "not-an-address" }),
        );
        assert!(invalid.error.is_some());
    }

    #[test]
    fn failed_reverse_core_function_records_adapter_error_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::new(Arc::new(FailingReverseCoreSupervisor));
        let session_id = open_reverse_session(&host, temp.path());

        let response = reverse_core_rpc(
            &host,
            "reverse.decompile",
            session_id,
            json!({ "addr": "0x140001000" }),
        );
        assert!(response.error.is_some());

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "reverse.adapter_error")
        );
        let operations = workspace.list_operations().unwrap();
        let failed = operations
            .iter()
            .find(|operation| operation.capability == "reverse.decompile")
            .unwrap();
        assert!(matches!(failed.status, OperationStatus::Failed));
    }

    #[test]
    fn debug_and_reverse_sessions_reject_cross_capability_calls() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let debug_id =
            create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
        let reverse_id = open_reverse_session(&host, temp.path());

        let reverse_with_debug_session = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "reverse.list_funcs".to_string(),
            params: Some(json!({
                "session_id": debug_id,
                "count": 1
            })),
        });
        assert!(reverse_with_debug_session.error.is_some());

        let debug_with_reverse_session = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "debug.eval".to_string(),
            params: Some(json!({
                "session_id": reverse_id,
                "command": ".echo wrong-domain"
            })),
        });
        assert!(debug_with_reverse_session.error.is_some());
    }

    #[test]
    fn failed_reverse_open_records_adapter_error_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.exe");
        fs::write(&database, b"sample").unwrap();
        let host = ServiceHost::new(Arc::new(FailingReverseOpenSupervisor));

        let open = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "reverse.session.open".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "database_path": database
            })),
        });
        assert!(open.error.is_some());

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        let error_artifact = artifacts
            .iter()
            .find(|artifact| artifact.kind == "reverse.adapter_error")
            .expect("reverse adapter error artifact is recorded");
        let error_path = workspace
            .resolve_artifact_relative_path(&error_artifact.relative_path)
            .unwrap();
        let error_text = fs::read_to_string(error_path).unwrap();
        assert!(error_text.contains("ida_error_kind=open_database_failed"));
        assert!(error_text.contains("possible_reason=database_locked_or_unavailable"));
        let operations = workspace.list_operations().unwrap();
        let failed = operations
            .iter()
            .find(|operation| operation.capability == "reverse.session.open")
            .unwrap();
        assert!(matches!(failed.status, OperationStatus::Failed));
        assert!(
            failed
                .summary
                .contains("ida_error_kind=open_database_failed")
        );
    }

    #[test]
    fn failed_reverse_open_records_worker_create_error_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.exe");
        fs::write(&database, b"sample").unwrap();
        let host = ServiceHost::new(Arc::new(FailingCreateWorkerSupervisor));

        let open = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "reverse.session.open".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "database_path": database
            })),
        });
        assert!(open.error.is_some());

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        let error_artifact = artifacts
            .iter()
            .find(|artifact| artifact.kind == "reverse.adapter_error")
            .expect("reverse worker create error artifact is recorded");
        let error_path = workspace
            .resolve_artifact_relative_path(&error_artifact.relative_path)
            .unwrap();
        let error_text = fs::read_to_string(error_path).unwrap();
        assert!(error_text.contains("failed to create reverse worker"));
        assert!(error_text.contains("os error 5"));
        let operations = workspace.list_operations().unwrap();
        let failed = operations
            .iter()
            .find(|operation| operation.capability == "reverse.session.open")
            .unwrap();
        assert!(matches!(failed.status, OperationStatus::Failed));
        assert!(failed.summary.contains("failed to create reverse worker"));
    }

    #[test]
    fn failed_reverse_close_keeps_session_reusable_and_records_failure() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.exe");
        fs::write(&database, b"sample").unwrap();
        let host = ServiceHost::new(Arc::new(FailingReverseCloseSupervisor));
        let open = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "reverse.session.open".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "database_path": database
            })),
        });
        let open_result = open.result.unwrap();
        let session_id = open_result["session_id"].clone();

        let close = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "reverse.session.close".to_string(),
            params: Some(json!({ "session_id": session_id })),
        });
        assert!(close.error.is_some());

        let lookup = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(4)),
            method: "reverse.lookup_function".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "runtime_address": 0x180001000u64,
                "runtime_module_base": 0x180000000u64,
                "ida_image_base": 0x140000000u64
            })),
        });
        assert!(lookup.error.is_none(), "{:?}", lookup.error);

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let close_operation = workspace
            .list_operations()
            .unwrap()
            .into_iter()
            .find(|operation| operation.capability == "reverse.session.close")
            .unwrap();
        assert!(matches!(close_operation.status, OperationStatus::Failed));
    }

    #[test]
    fn closing_reverse_session_removes_session_from_service_state() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let session_id = open_reverse_session(&host, temp.path());

        let close = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "reverse.session.close".to_string(),
            params: Some(json!({ "session_id": session_id.clone() })),
        });
        assert!(close.error.is_none(), "{:?}", close.error);

        let lookup = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(4)),
            method: "reverse.lookup_function".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "runtime_address": 0x180001000u64,
                "runtime_module_base": 0x180000000u64,
                "ida_image_base": 0x140000000u64
            })),
        });
        assert_eq!(lookup.error.unwrap().code, -32010);
    }

    #[test]
    fn recording_start_creates_recording_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let response = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.as_ref().unwrap();
        assert!(result.get("recording_id").is_some());
        assert_eq!(result["state"], "running");
        assert_eq!(result["operation_status"], "success");
        assert_eq!(result["artifact_refs"].as_array().unwrap().len(), 2);

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "recording.metadata")
        );
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "recording.events.process")
        );
        let recording_id = result["recording_id"]["id"].as_str().unwrap();
        assert!(
            workspace
                .root()
                .join("artifacts")
                .join("recordings")
                .join(recording_id)
                .join("recording.json")
                .is_file()
        );
    }

    #[test]
    fn recording_start_records_selected_etw_preset_flags() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "recording.start".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": { "type": "attach", "pid": 42 },
                "presets": ["process", "network"]
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.as_ref().unwrap();
        let recording_id = result["recording_id"]["id"].as_str().unwrap();
        let recording_json = fs::read_to_string(
            temp.path()
                .join(INTERNAL_WORKSPACE_DIR)
                .join("artifacts")
                .join("recordings")
                .join(recording_id)
                .join("recording.json"),
        )
        .unwrap();
        let metadata: Value = serde_json::from_str(&recording_json).unwrap();
        let expected_flags = dbgatlas_etw::EtwPresetFlags::PROCESS.bits()
            | dbgatlas_etw::EtwPresetFlags::NETWORK.bits();

        assert_eq!(metadata["presets"], json!(["process", "network"]));
        assert_eq!(metadata["etw_preset_flags"], json!(expected_flags));
        assert!(metadata.get("trace_consumer_error").is_some());
        assert_eq!(
            metadata["adapter"]["capabilities"]["realtime_consume"],
            true
        );
        assert_eq!(
            metadata["adapter"]["capabilities"]["event_stack_trace"],
            true
        );
        assert_eq!(metadata["stack_trace"]["requested"], true);
        assert!(metadata["stack_trace"]["enabled"].is_boolean());
        assert!(metadata["stack_trace"]["warnings"].is_array());
    }

    #[test]
    fn recording_ttd_registers_run_trace_and_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let host = ttd_test_host(MockTtdRunnerMode::CompletedWithTrace);
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "recording.ttd".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": { "kind": "attach", "pid": 42 },
                "timeout_ms": 1000,
                "options": { "accept_eula": true }
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["state"], "completed");
        assert_eq!(result["operation_status"], "success");
        assert_eq!(result["target_pid"], 42);
        assert!(
            result["primary_trace_path"]
                .as_str()
                .unwrap()
                .ends_with("sample.run")
        );
        assert_eq!(result["trace_paths"].as_array().unwrap().len(), 1);
        assert_eq!(result["trace_index_paths"].as_array().unwrap().len(), 1);
        assert_eq!(result["trace_artifacts"].as_array().unwrap().len(), 1);
        let trace_relative_path = result["trace_artifacts"][0]["relative_path"]
            .as_str()
            .unwrap()
            .replace('/', "\\");
        assert_eq!(
            trace_relative_path,
            format!(
                "artifacts\\recordings\\{}\\traces\\sample.run",
                result["recording_id"]["id"].as_str().unwrap()
            )
        );

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "recording.ttd.trace")
        );
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "recording.ttd.index")
        );
        let recording_id = result["recording_id"]["id"].as_str().unwrap();
        let metadata: Value = serde_json::from_str(
            &fs::read_to_string(
                workspace
                    .root()
                    .join("artifacts")
                    .join("recordings")
                    .join(recording_id)
                    .join("recording.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["status"], "completed");
        assert_eq!(metadata["mode"], "attach");
        assert_eq!(metadata["traces"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn recording_ttd_attach_uses_attach_args_and_requested_identity() {
        let temp = tempfile::tempdir().unwrap();
        let (host, invocations) =
            ttd_test_host_and_invocations(MockTtdRunnerMode::CompletedWithTrace);
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "recording.ttd".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": { "kind": "attach", "pid": 4242 },
                "timeout_ms": 1000,
                "worker_identity": "active_interactive_user"
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["mode"], "attach");
        assert_eq!(result["worker_identity"], "active_interactive_user");
        let invocations = invocations.lock().unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(
            invocations[0].worker_identity,
            TtdRecorderIdentity::ActiveInteractiveUser
        );
        let args = invocations[0]
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.contains(&"-attach".to_string()));
        assert!(args.contains(&"4242".to_string()));
        assert!(!args.contains(&"-launch".to_string()));

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let recording_id = result["recording_id"]["id"].as_str().unwrap();
        let metadata: Value = serde_json::from_str(
            &fs::read_to_string(
                workspace
                    .root()
                    .join("artifacts")
                    .join("recordings")
                    .join(recording_id)
                    .join("recording.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["target"]["kind"], "attach");
        assert_eq!(metadata["target"]["pid"], 4242);
        assert_eq!(metadata["worker_identity"], "active_interactive_user");
        assert_eq!(
            metadata["adapter"]["worker_identity"],
            "active_interactive_user"
        );
    }

    #[test]
    fn ttd_active_user_helper_args_preserve_recorder_args_after_separator() {
        let args = vec![
            OsString::from("-out"),
            OsString::from(r"C:\case\traces"),
            OsString::from("-attach"),
            OsString::from("4242"),
        ];
        let helper_args = ttd_command_helper_args(
            Path::new(r"C:\ttd\TTD.exe"),
            &args,
            Path::new(r"C:\case\recorder.stdout.txt"),
            Path::new(r"C:\case\recorder.stderr.txt"),
        );
        let text = helper_args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(text[0], "service");
        assert_eq!(text[1], "run-ttd-command");
        let separator = text.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(
            &text[separator + 1..],
            ["-out", r"C:\case\traces", "-attach", "4242"]
        );
    }

    #[test]
    fn ttd_timeout_stop_args_do_not_mix_stop_and_wait() {
        let args = ttd_stop_args(OsStr::new("4242"))
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args, ["-stop", "4242"]);
    }

    #[test]
    fn recording_ttd_timeout_invokes_stop_and_keeps_trace() {
        let temp = tempfile::tempdir().unwrap();
        let host = ttd_test_host(MockTtdRunnerMode::TimedOutWithTrace);
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "recording.ttd".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": { "kind": "launch", "executable": "app.exe", "args": [] },
                "timeout_ms": 1000
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["state"], "timed_out");
        assert_eq!(result["operation_status"], "success");

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(artifacts.iter().any(|artifact| {
            artifact.relative_path.ends_with("recorder-stop.stdout.txt")
                && artifact.kind == "recording.recorder_output"
        }));
    }

    #[test]
    fn recording_ttd_without_run_trace_is_failed() {
        let temp = tempfile::tempdir().unwrap();
        let host = ttd_test_host(MockTtdRunnerMode::CompletedWithoutTrace);
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "recording.ttd".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": { "kind": "attach", "pid": 42 },
                "timeout_ms": 1000
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["state"], "failed");
        assert_eq!(result["operation_status"], "failed");
        assert!(result["error"].as_str().unwrap().contains("no .run trace"));
    }

    #[test]
    fn recording_launch_starts_target_and_records_root_pid() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let (executable, args) = trivial_recording_command();
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "recording.start".to_string(),
            params: Some(json!({
                "project_root": temp.path(),
                "target": {
                    "type": "launch",
                    "executable": executable,
                    "args": args
                },
                "presets": ["process"]
            })),
        });

        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.as_ref().unwrap();
        let root_pid = result["root_pid"].as_u64().unwrap();
        assert!(root_pid > 0);

        let recording_id = result["recording_id"]["id"].as_str().unwrap();
        let recording_json = fs::read_to_string(
            temp.path()
                .join(INTERNAL_WORKSPACE_DIR)
                .join("artifacts")
                .join("recordings")
                .join(recording_id)
                .join("recording.json"),
        )
        .unwrap();
        let metadata: Value = serde_json::from_str(&recording_json).unwrap();
        assert_eq!(metadata["mode"], "launch");
        assert_eq!(metadata["root_pid"], json!(root_pid));
    }

    #[test]
    fn recording_stop_registers_trace_and_category_events() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
        let recording_id = start.result.unwrap()["recording_id"].clone();

        let stop = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "recording.stop".to_string(),
            params: Some(json!({ "recording_id": recording_id.clone() })),
        });

        assert!(stop.error.is_none(), "{:?}", stop.error);
        let result = stop.result.as_ref().unwrap();
        assert_eq!(result["state"], "stopped");
        assert_eq!(result["operation_status"], "success");

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "recording.trace")
        );
        for category in ["process", "thread", "image", "file", "registry", "network"] {
            let kind = format!("recording.events.{category}");
            assert!(
                artifacts.iter().any(|artifact| artifact.kind == kind),
                "missing artifact kind {kind}"
            );
        }
        let operations = workspace.list_operations().unwrap();
        assert!(
            operations
                .iter()
                .any(|operation| operation.capability == "recording.stop"
                    && matches!(operation.status, OperationStatus::Success))
        );
        let recording_dir = workspace
            .root()
            .join("artifacts")
            .join("recordings")
            .join(recording_id["id"].as_str().unwrap());
        let recording_metadata: Value = serde_json::from_str(
            &fs::read_to_string(recording_dir.join("recording.json")).unwrap(),
        )
        .unwrap();
        assert!(recording_metadata["trace"]["valid_etl"].is_boolean());
        assert!(recording_metadata["trace"].get("fallback_reason").is_some());
        assert!(recording_metadata.get("event_extraction").is_some());
        assert!(
            recording_metadata["event_extraction"].is_null()
                || recording_metadata["event_extraction"]["warnings"].is_array()
        );
        assert!(recording_dir.join("trace.etl").is_file());
        assert!(recording_dir.join("events").join("network.jsonl").is_file());
        for category in ["process", "thread", "image", "file", "registry", "network"] {
            let path = recording_dir
                .join("events")
                .join(format!("{category}.jsonl"));
            let text = fs::read_to_string(path).unwrap();
            let first_line = text.lines().next().unwrap();
            let event: Value = serde_json::from_str(first_line).unwrap();
            assert_eq!(event["schema_version"], 1);
            assert_eq!(event["category"], category);
            assert!(event.get("timestamp").is_some());
            assert!(event.get("event_type").is_some());
            assert!(event.get("pid").is_some());
            assert!(event.get("tid").is_some());
            assert!(event.get("process").is_some());
            assert!(event.get("etw").is_some());
            if let Some(stack) = event.get("stack") {
                let frames = stack["frames"].as_array().unwrap();
                assert!(frames.iter().all(|frame| frame.is_string()));
            }
        }
    }

    #[test]
    fn recording_workspace_facts_exposes_reportable_event_materials() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
        let recording_id = start.result.unwrap()["recording_id"].clone();

        let stop = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "recording.stop".to_string(),
            params: Some(json!({ "recording_id": recording_id.clone() })),
        });
        assert!(stop.error.is_none(), "{:?}", stop.error);
        let stop_operation_id =
            serde_json::from_value::<OperationRef>(stop.result.unwrap()["operation_id"].clone())
                .unwrap();

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let facts = workspace.facts().unwrap();
        assert!(facts.operations.iter().any(|operation| {
            operation.operation_id == stop_operation_id
                && operation.capability == "recording.stop"
                && matches!(operation.status, OperationStatus::Success)
        }));

        let recording_id = recording_id["id"].as_str().unwrap();
        let expected = [
            ("recording.metadata", "recording.json"),
            ("recording.trace", "trace.etl"),
            ("recording.events.process", "events/process.jsonl"),
            ("recording.events.thread", "events/thread.jsonl"),
            ("recording.events.image", "events/image.jsonl"),
            ("recording.events.file", "events/file.jsonl"),
            ("recording.events.registry", "events/registry.jsonl"),
            ("recording.events.network", "events/network.jsonl"),
        ];
        for (kind, suffix) in expected {
            let relative_path =
                PathBuf::from(format!("artifacts/recordings/{recording_id}/{suffix}"));
            let artifact = facts
                .artifacts
                .iter()
                .find(|artifact| {
                    artifact.kind == kind
                        && artifact.relative_path == relative_path
                        && artifact.operation_id.as_ref() == Some(&stop_operation_id)
                })
                .unwrap_or_else(|| panic!("missing reportable artifact {kind} at {suffix}"));
            assert!(artifact.byte_len.unwrap_or_default() > 0);
        }

        let process_events = workspace
            .root()
            .join("artifacts")
            .join("recordings")
            .join(recording_id)
            .join("events")
            .join("process.jsonl");
        let first_line = fs::read_to_string(process_events)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        let event: Value = serde_json::from_str(&first_line).unwrap();
        assert_eq!(event["recording_id"]["id"], recording_id);
        assert_eq!(event["schema_version"], 1);
        assert_eq!(event["category"], "process");
    }

    #[test]
    fn recording_cancel_is_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
        let recording_id = start.result.unwrap()["recording_id"].clone();

        let cancel = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "recording.cancel".to_string(),
            params: Some(json!({ "recording_id": recording_id.clone() })),
        });
        assert!(cancel.error.is_none(), "{:?}", cancel.error);
        assert_eq!(cancel.result.unwrap()["operation_status"], "canceled");

        let stop = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "recording.stop".to_string(),
            params: Some(json!({ "recording_id": recording_id })),
        });
        assert_eq!(stop.error.unwrap().code, -32016);

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let operations = workspace.list_operations().unwrap();
        let cancel_operation = operations
            .iter()
            .find(|operation| operation.capability == "recording.cancel")
            .unwrap();
        assert!(matches!(cancel_operation.status, OperationStatus::Canceled));
        let artifacts = workspace.list_artifacts().unwrap();
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "recording.trace")
        );
    }

    #[test]
    fn recording_kill_records_failed_operation_and_keeps_artifacts_in_facts() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
        let recording_id = start.result.unwrap()["recording_id"].clone();

        let kill = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "recording.kill".to_string(),
            params: Some(json!({ "recording_id": recording_id.clone() })),
        });

        assert!(kill.error.is_none(), "{:?}", kill.error);
        let result = kill.result.unwrap();
        assert_eq!(result["state"], "killed");
        assert_eq!(result["operation_status"], "failed");
        let kill_operation_id =
            serde_json::from_value::<OperationRef>(result["operation_id"].clone()).unwrap();

        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        let facts = workspace.facts().unwrap();
        assert!(facts.operations.iter().any(|operation| {
            operation.capability == "recording.kill"
                && matches!(operation.status, OperationStatus::Failed)
        }));
        let recording_id = recording_id["id"].as_str().unwrap();
        let recording_json = PathBuf::from(format!(
            "artifacts/recordings/{recording_id}/recording.json"
        ));
        let trace = PathBuf::from(format!("artifacts/recordings/{recording_id}/trace.etl"));
        assert!(
            facts
                .artifacts
                .iter()
                .any(|artifact| artifact.relative_path == recording_json
                    && artifact.kind == "recording.metadata"
                    && artifact.operation_id.as_ref() == Some(&kill_operation_id))
        );
        assert!(
            facts
                .artifacts
                .iter()
                .any(|artifact| artifact.relative_path == trace
                    && artifact.kind == "recording.trace"
                    && artifact.operation_id.as_ref() == Some(&kill_operation_id))
        );
        let recording_metadata: Value = serde_json::from_str(
            &fs::read_to_string(workspace.root().join(recording_json)).unwrap(),
        )
        .unwrap();
        assert_eq!(recording_metadata["state"], "killed");
        assert_eq!(
            recording_metadata["operation_id"],
            serde_json::to_value(kill_operation_id).unwrap()
        );
    }

    #[test]
    fn recording_status_uses_public_shape() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
        let recording_id = start.result.unwrap()["recording_id"].clone();

        let status = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "recording.status".to_string(),
            params: Some(json!({ "recording_id": recording_id })),
        });

        assert!(status.error.is_none(), "{:?}", status.error);
        let result = status.result.unwrap();
        assert_eq!(result["state"], "running");
        assert!(result.get("artifact_refs").is_some());
        assert!(result.get("last_operation").is_some());
        assert!(result.get("project_root").is_none());
        assert!(result.get("internal_workspace_root").is_none());
        assert!(result.get("artifact_dir").is_none());
    }

    #[test]
    fn repeated_recording_stop_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
        let recording_id = start.result.unwrap()["recording_id"].clone();

        let first = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "recording.stop".to_string(),
            params: Some(json!({ "recording_id": recording_id.clone() })),
        });
        assert!(first.error.is_none(), "{:?}", first.error);

        let second = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "recording.stop".to_string(),
            params: Some(json!({ "recording_id": recording_id })),
        });
        assert_eq!(second.error.unwrap().code, -32016);
    }

    #[test]
    fn concurrent_recording_finish_allows_one_terminal_operation() {
        let temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
        let recording_id = start.result.unwrap()["recording_id"].clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let stop_host = host.clone();
        let cancel_host = host.clone();
        let stop_recording = recording_id.clone();
        let cancel_recording = recording_id;
        let stop_barrier = barrier.clone();
        let cancel_barrier = barrier;

        let stop = std::thread::spawn(move || {
            stop_barrier.wait();
            stop_host.handle_rpc(JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(json!(2)),
                method: "recording.stop".to_string(),
                params: Some(json!({ "recording_id": stop_recording })),
            })
        });
        let cancel = std::thread::spawn(move || {
            cancel_barrier.wait();
            cancel_host.handle_rpc(JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(json!(3)),
                method: "recording.cancel".to_string(),
                params: Some(json!({ "recording_id": cancel_recording })),
            })
        });
        let responses = vec![stop.join().unwrap(), cancel.join().unwrap()];
        let success_count = responses
            .iter()
            .filter(|response| response.error.is_none())
            .count();
        let terminal_reject_count = responses
            .iter()
            .filter(|response| {
                response
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == -32016)
            })
            .count();

        assert_eq!(success_count, 1);
        assert_eq!(terminal_reject_count, 1);
    }

    #[test]
    fn rejects_missing_bearer_token() {
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/rpc".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
        };
        let error = authorize_http_request(&request, &ServiceConfig::dev_default()).unwrap_err();
        assert!(matches!(error, ServiceError::Unauthorized));
    }

    #[test]
    fn validates_loopback_service_bind() {
        let config = ServiceConfig {
            bind: "0.0.0.0:7331".parse().unwrap(),
            bearer_token: "token".to_string(),
        };

        assert!(matches!(
            validate_config(&config),
            Err(ServiceError::NonLoopbackBind(_))
        ));
    }

    #[test]
    fn origin_check_requires_exact_loopback_host() {
        assert!(is_allowed_origin("http://localhost:7331"));
        assert!(is_allowed_origin("http://127.0.0.1:7331"));
        assert!(is_allowed_origin("http://[::1]:7331"));
        assert!(!is_allowed_origin("http://localhost.evil.test"));
        assert!(!is_allowed_origin("http://127.0.0.1.evil.test"));
        assert!(!is_allowed_origin("http://[::1].evil.test"));
    }

    #[test]
    fn http_mcp_initialize_returns_capabilities() {
        let server = start_mock_http_service();
        let response = post_json(
            server.endpoint,
            "/mcp",
            Some("test-token"),
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {}
            }),
        );

        assert_eq!(http_status(&response), 200);
        let body = http_body_json(&response);
        assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(body["result"]["serverInfo"]["name"], "dbgatlas-mcp");
        assert!(body["result"]["capabilities"].get("tools").is_some());
        server.stop();
    }

    #[test]
    fn http_mcp_tools_list_returns_current_tools() {
        let server = start_mock_http_service();
        let response = post_json(
            server.endpoint,
            "/mcp",
            Some("test-token"),
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }),
        );

        assert_eq!(http_status(&response), 200);
        let body = http_body_json(&response);
        let tools = body["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "debug.eval"));
        let service_update = tools
            .iter()
            .find(|tool| tool["name"] == "service.update")
            .expect("service.update tool is listed");
        assert_eq!(
            service_update["inputSchema"]["properties"]["source_dir"]["type"],
            "string"
        );
        assert_eq!(
            service_update["inputSchema"]["properties"]["restart"]["default"],
            true
        );
        for tool_name in [
            "reverse.lookup_funcs",
            "reverse.int_convert",
            "reverse.list_funcs",
            "reverse.list_globals",
            "reverse.list_imports",
            "reverse.list_strings",
            "reverse.get_string",
            "reverse.get_bytes",
            "reverse.get_int",
            "reverse.decompile",
            "reverse.disasm",
            "reverse.xrefs_to",
            "reverse.xrefs_to_field",
            "reverse.callees",
            "reverse.rename",
            "reverse.set_comments",
            "reverse.set_type",
            "reverse.declare_type",
            "reverse.inspect_item",
            "reverse.force_recompile",
            "reverse.idb_save",
            "reverse.find_bytes",
            "reverse.search_text",
            "reverse.query_xrefs",
            "reverse.query_funcs",
            "reverse.query_entities",
        ] {
            assert!(
                tools.iter().any(|tool| tool["name"] == tool_name),
                "{tool_name} tool is listed"
            );
        }
        for legacy_tool_name in [
            "reverse.imports",
            "reverse.xref_query",
            "reverse.func_query",
            "reverse.entity_query",
        ] {
            assert!(
                !tools.iter().any(|tool| tool["name"] == legacy_tool_name),
                "{legacy_tool_name} legacy tool name is not listed"
            );
        }
        assert!(tools.iter().any(|tool| tool["name"] == "workspace.facts"));
        assert!(tools.iter().any(|tool| tool["name"] == "recording.ttd"));
        assert!(!tools.iter().any(|tool| tool["name"] == "reverse.py_eval"));
        assert!(!tools.iter().any(|tool| tool["name"] == "recording.start"));
        server.stop();
    }

    #[test]
    fn http_mcp_tools_list_includes_py_eval_when_enabled() {
        let server = start_mock_http_service_with_host(
            ServiceHost::with_mock_workers().with_ida_py_eval(true),
        );
        let response = post_json(
            server.endpoint,
            "/mcp",
            Some("test-token"),
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }),
        );

        assert_eq!(http_status(&response), 200);
        let body = http_body_json(&response);
        let tools = body["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "reverse.py_eval"));
        server.stop();
    }

    #[test]
    fn http_mcp_py_eval_call_is_disabled_by_default() {
        let server = start_mock_http_service();
        let response = post_json(
            server.endpoint,
            "/mcp",
            Some("test-token"),
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "reverse.py_eval",
                    "arguments": {
                        "session_id": { "kind": "reverse", "id": "reverse-1" },
                        "code": "print('should not run')"
                    }
                }
            }),
        );

        assert_eq!(http_status(&response), 200);
        let body = http_body_json(&response);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("disabled by runtime policy")
        );
        server.stop();
    }

    #[test]
    fn http_mcp_ping_returns_empty_result() {
        let server = start_mock_http_service();
        let response = post_json(
            server.endpoint,
            "/mcp",
            Some("test-token"),
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "ping",
                "params": {}
            }),
        );

        assert_eq!(http_status(&response), 200);
        assert_eq!(http_body_json(&response)["result"], json!({}));
        server.stop();
    }

    #[test]
    fn http_mcp_notifications_return_accepted_without_body() {
        let server = start_mock_http_service();
        let response = post_json(
            server.endpoint,
            "/mcp",
            Some("test-token"),
            None,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
        );

        assert_eq!(http_status(&response), 202);
        assert!(response.ends_with("\r\n\r\n"));
        server.stop();
    }

    #[test]
    fn http_mcp_debug_workflow_uses_service_results_with_refs() {
        let temp = tempfile::tempdir().unwrap();
        let server = start_mock_http_service();
        let create = mcp_tool_call(
            server.endpoint,
            "debug.session.create",
            json!({
                "project_root": temp.path(),
                "target": { "kind": "file", "path": "sample.dmp" }
            }),
        );
        let session_id = create["session_id"].clone();
        let eval = mcp_tool_call(
            server.endpoint,
            "debug.eval",
            json!({
                "session_id": session_id,
                "command": ".echo from-http-mcp"
            }),
        );

        assert_eq!(eval["operation_status"], "success");
        assert!(eval["raw_output_ref"].get("id").is_some());
        assert_eq!(eval["artifact_refs"].as_array().unwrap().len(), 3);
        server.stop();
    }

    #[test]
    fn http_mcp_reverse_core_function_uses_service_results_with_refs() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.exe");
        fs::write(&database, b"sample").unwrap();
        let server = start_mock_http_service();
        let create = mcp_tool_call(
            server.endpoint,
            "debug.session.create",
            json!({
                "project_root": temp.path(),
                "target": { "kind": "file", "path": "sample.dmp" }
            }),
        );
        assert!(create.get("session_id").is_some());
        let open = mcp_tool_call(
            server.endpoint,
            "reverse.session.open",
            json!({
                "project_root": temp.path(),
                "database_path": database
            }),
        );
        let result = mcp_tool_call(
            server.endpoint,
            "reverse.list_funcs",
            json!({
                "session_id": open["session_id"],
                "offset": 0,
                "count": 1
            }),
        );

        assert_eq!(result["operation_status"], "success");
        assert_eq!(result["function"], "list_funcs");
        assert_eq!(result["artifact_refs"].as_array().unwrap().len(), 1);
        assert_eq!(result["result"]["count"], 1);
        server.stop();
    }

    #[test]
    fn http_mcp_reverse_query_funcs_sort_by_returns_tool_result() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.exe");
        fs::write(&database, b"sample").unwrap();
        let server = start_mock_http_service();
        let open = mcp_tool_call(
            server.endpoint,
            "reverse.session.open",
            json!({
                "project_root": temp.path(),
                "database_path": database
            }),
        );
        let result = mcp_tool_call(
            server.endpoint,
            "reverse.query_funcs",
            json!({
                "session_id": open["session_id"],
                "filter": "parse",
                "sort_by": "name",
                "offset": 0,
                "count": 20
            }),
        );

        assert_eq!(result["operation_status"], "success");
        assert_eq!(result["function"], "query_funcs");
        assert_eq!(result["result"]["items"][0]["name"], "parse_args");
        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        assert!(
            workspace
                .list_operations()
                .unwrap()
                .iter()
                .any(|operation| {
                    operation.capability == "reverse.query_funcs"
                        && matches!(operation.status, OperationStatus::Success)
                })
        );
        server.stop();
    }

    #[test]
    fn http_mcp_reverse_core_error_is_tool_error() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.exe");
        fs::write(&database, b"sample").unwrap();
        let server = start_mock_http_service_with_host(ServiceHost::new(Arc::new(
            FailingReverseCoreSupervisor,
        )));
        let open = mcp_tool_call(
            server.endpoint,
            "reverse.session.open",
            json!({
                "project_root": temp.path(),
                "database_path": database
            }),
        );
        let body = mcp_tool_call_body(
            server.endpoint,
            "reverse.decompile",
            json!({
                "session_id": open["session_id"],
                "addr": "0x140001000"
            }),
        );

        assert_eq!(body["result"]["isError"], true, "{body}");
        let text: Value =
            serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["error"]["code"], -32000);
        assert!(
            text["error"]["message"]
                .as_str()
                .unwrap()
                .contains("mock IDA core function failed")
        );
        assert!(
            text["operation_id"]["id"]
                .as_str()
                .unwrap()
                .starts_with("op-")
        );
        let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
        assert!(
            workspace
                .list_artifacts()
                .unwrap()
                .iter()
                .any(|artifact| artifact.kind == "reverse.adapter_error")
        );
        assert!(
            workspace
                .list_operations()
                .unwrap()
                .iter()
                .any(|operation| {
                    operation.capability == "reverse.decompile"
                        && matches!(operation.status, OperationStatus::Failed)
                })
        );
        server.stop();
    }

    #[test]
    fn http_mcp_workspace_facts_reads_recording_layer() {
        let temp = tempfile::tempdir().unwrap();
        let server = start_mock_http_service();
        let create = mcp_tool_call(
            server.endpoint,
            "debug.session.create",
            json!({
                "project_root": temp.path(),
                "target": { "kind": "file", "path": "sample.dmp" }
            }),
        );
        let session_id = create["session_id"].clone();
        mcp_tool_call(
            server.endpoint,
            "debug.eval",
            json!({
                "session_id": session_id,
                "command": ".echo facts"
            }),
        );

        let facts = mcp_tool_call(
            server.endpoint,
            "workspace.facts",
            json!({ "path": temp.path().join(INTERNAL_WORKSPACE_DIR) }),
        );

        assert_eq!(facts["command_audit"].as_array().unwrap().len(), 1);
        assert!(
            facts["operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation["capability"] == "debug.eval")
        );
        server.stop();
    }

    #[test]
    fn workspace_facts_falls_back_to_project_artifacts_without_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp
            .path()
            .join(INTERNAL_WORKSPACE_DIR)
            .join("artifacts")
            .join("recordings")
            .join("recording-1")
            .join("traces")
            .join("sample.run");
        fs::create_dir_all(trace.parent().unwrap()).unwrap();
        fs::write(&trace, b"run").unwrap();

        let facts = workspace_facts_with_fallback(temp.path()).unwrap();

        let artifact = facts
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "recording.ttd.trace")
            .unwrap();
        assert_eq!(artifact.byte_len, Some(3));
        assert!(
            artifact
                .artifact_id
                .id
                .as_str()
                .starts_with("artifact-synthetic-")
        );
    }

    #[test]
    fn http_mcp_rejects_missing_bearer_token() {
        let server = start_mock_http_service();
        let response = post_json(
            server.endpoint,
            "/mcp",
            None,
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }),
        );

        assert_eq!(http_status(&response), 401);
        assert_eq!(http_body_json(&response)["error"]["code"], -32001);
        server.stop();
    }

    #[test]
    fn http_mcp_rejects_non_loopback_origin() {
        let server = start_mock_http_service();
        let response = post_json(
            server.endpoint,
            "/mcp",
            Some("test-token"),
            Some("http://localhost.evil.test"),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }),
        );

        assert_eq!(http_status(&response), 403);
        assert_eq!(http_body_json(&response)["error"]["code"], -32002);
        server.stop();
    }

    #[test]
    fn http_rpc_path_still_returns_health() {
        let server = start_mock_http_service();
        let response = post_json(
            server.endpoint,
            "/rpc",
            Some("test-token"),
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "service.health",
                "params": {}
            }),
        );

        assert_eq!(http_status(&response), 200);
        assert_eq!(http_body_json(&response)["result"]["status"], "ok");
        server.stop();
    }

    struct TestServer {
        endpoint: SocketAddr,
        shutdown: ServiceShutdown,
        handle: std::thread::JoinHandle<Result<(), ServiceError>>,
    }

    impl TestServer {
        fn stop(self) {
            self.shutdown.request_stop();
            self.handle.join().unwrap().unwrap();
        }
    }

    fn start_mock_http_service() -> TestServer {
        start_mock_http_service_with_host(ServiceHost::with_mock_workers())
    }

    fn start_mock_http_service_with_host(host: ServiceHost) -> TestServer {
        let endpoint = unused_loopback_endpoint();
        let shutdown = ServiceShutdown::new();
        let server_shutdown = shutdown.clone();
        let handle = std::thread::spawn(move || {
            run_http_service_until(
                ServiceConfig {
                    bind: endpoint,
                    bearer_token: "test-token".to_string(),
                },
                host,
                server_shutdown,
            )
        });
        wait_for_service(endpoint);
        TestServer {
            endpoint,
            shutdown,
            handle,
        }
    }

    fn unused_loopback_endpoint() -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = listener.local_addr().unwrap();
        drop(listener);
        endpoint
    }

    fn wait_for_service(endpoint: SocketAddr) {
        for _ in 0..50 {
            if TcpStream::connect(endpoint).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("service did not start at {endpoint}");
    }

    fn post_json(
        endpoint: SocketAddr,
        path: &str,
        token: Option<&str>,
        origin: Option<&str>,
        body: Value,
    ) -> String {
        let body = serde_json::to_string(&body).unwrap();
        let mut stream = TcpStream::connect(endpoint).unwrap();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: {endpoint}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        )
        .unwrap();
        if let Some(token) = token {
            write!(stream, "Authorization: Bearer {token}\r\n").unwrap();
        }
        if let Some(origin) = origin {
            write!(stream, "Origin: {origin}\r\n").unwrap();
        }
        write!(stream, "\r\n{body}").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn http_status(response: &str) -> u16 {
        response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .unwrap()
    }

    fn http_body_json(response: &str) -> Value {
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap()
    }

    fn mcp_tool_call_body(endpoint: SocketAddr, name: &str, arguments: Value) -> Value {
        let response = post_json(
            endpoint,
            "/mcp",
            Some("test-token"),
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": arguments
                }
            }),
        );
        assert_eq!(http_status(&response), 200);
        http_body_json(&response)
    }

    fn mcp_tool_call(endpoint: SocketAddr, name: &str, arguments: Value) -> Value {
        let body = mcp_tool_call_body(endpoint, name, arguments);
        assert_eq!(body["result"]["isError"], false, "{body}");
        serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn create_debug_session(host: &ServiceHost, project_root: &Path) -> JsonRpcResponse {
        host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "debug.session.create".to_string(),
            params: Some(json!({
                "project_root": project_root,
                "target": { "kind": "file", "path": "sample.dmp" }
            })),
        })
    }

    fn open_reverse_session(host: &ServiceHost, project_root: &Path) -> Value {
        let database = project_root.join("sample.exe");
        fs::write(&database, b"sample").unwrap();
        let open = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "reverse.session.open".to_string(),
            params: Some(json!({
                "project_root": project_root,
                "database_path": database
            })),
        });
        assert!(open.error.is_none(), "{:?}", open.error);
        let result = open.result.unwrap();
        result["session_id"].clone()
    }

    fn reverse_core_rpc(
        host: &ServiceHost,
        method: &str,
        session_id: Value,
        mut arguments: Value,
    ) -> JsonRpcResponse {
        arguments["session_id"] = session_id;
        host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: method.to_string(),
            params: Some(arguments),
        })
    }

    fn create_recording(host: &ServiceHost, project_root: &Path, target: Value) -> JsonRpcResponse {
        host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "recording.start".to_string(),
            params: Some(json!({
                "project_root": project_root,
                "target": target
            })),
        })
    }

    #[cfg(windows)]
    fn trivial_recording_command() -> (PathBuf, Vec<String>) {
        (
            PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            vec!["/C".to_string(), "exit 0".to_string()],
        )
    }

    #[cfg(not(windows))]
    fn trivial_recording_command() -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), "exit 0".to_string()],
        )
    }

    fn eval_request(host: &ServiceHost, session_id: Value, command: &str) -> JsonRpcResponse {
        host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval".to_string(),
            params: Some(json!({
                "session_id": session_id,
                "command": command
            })),
        })
    }

    fn close_request(host: &ServiceHost, session_id: Value) -> JsonRpcResponse {
        host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "debug.session.close".to_string(),
            params: Some(json!({ "session_id": session_id })),
        })
    }

    #[derive(Clone, Copy)]
    enum MockTtdRunnerMode {
        CompletedWithTrace,
        TimedOutWithTrace,
        CompletedWithoutTrace,
    }

    struct MockTtdRunner {
        mode: MockTtdRunnerMode,
        invocations: Arc<Mutex<Vec<TtdRecorderInvocation>>>,
        _temp: tempfile::TempDir,
    }

    impl TtdRecorderRunner for MockTtdRunner {
        fn run(&self, invocation: TtdRecorderInvocation) -> Result<TtdProcessExit, ServiceError> {
            self.invocations.lock().unwrap().push(invocation.clone());
            fs::write(
                &invocation.stdout_path,
                "Recording process sample.exe (42)\n",
            )?;
            fs::write(&invocation.stderr_path, "")?;
            let timed_out = matches!(self.mode, MockTtdRunnerMode::TimedOutWithTrace);
            if matches!(
                self.mode,
                MockTtdRunnerMode::CompletedWithTrace | MockTtdRunnerMode::TimedOutWithTrace
            ) {
                let traces_dir = ttd_out_dir(&invocation.args);
                fs::create_dir_all(&traces_dir)?;
                fs::write(traces_dir.join("sample.run"), b"run")?;
                fs::write(traces_dir.join("sample.idx"), b"idx")?;
            }
            let stop = if timed_out {
                let timeout_stop = invocation
                    .timeout_stop
                    .as_ref()
                    .expect("timeout invocation has stop target");
                fs::write(&timeout_stop.stdout_path, "stopped\n")?;
                fs::write(&timeout_stop.stderr_path, "")?;
                Some(TtdStopExit {
                    stop_target: timeout_stop.stop_target.clone(),
                    exit_code: Some(0),
                    timed_out: false,
                    error: None,
                })
            } else {
                assert!(invocation.timeout_stop.is_some());
                None
            };
            Ok(TtdProcessExit {
                exit_code: Some(0),
                timed_out,
                stop,
                killed_after_timeout: false,
            })
        }
    }

    fn ttd_test_host(mode: MockTtdRunnerMode) -> ServiceHost {
        ttd_test_host_and_invocations(mode).0
    }

    fn ttd_test_host_and_invocations(
        mode: MockTtdRunnerMode,
    ) -> (ServiceHost, Arc<Mutex<Vec<TtdRecorderInvocation>>>) {
        let temp = tempfile::tempdir().unwrap();
        let ttd_dir = temp.path().join("ttd");
        fs::create_dir_all(&ttd_dir).unwrap();
        fs::write(ttd_dir.join("TTD.exe"), b"fake").unwrap();
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let host = ServiceHost::with_mock_workers()
            .with_capabilities(ServiceCapabilities {
                ttd_dir: Some(ttd_dir),
                ..Default::default()
            })
            .with_ttd_runner(Arc::new(MockTtdRunner {
                mode,
                invocations: invocations.clone(),
                _temp: temp,
            }));
        (host, invocations)
    }

    fn ttd_out_dir(args: &[OsString]) -> PathBuf {
        args.windows(2)
            .find_map(|pair| (pair[0] == OsString::from("-out")).then(|| PathBuf::from(&pair[1])))
            .expect("TTD args contain -out")
    }

    struct RecordingIdentitySupervisor {
        identities: Mutex<Vec<Option<WorkerIdentity>>>,
    }

    struct AccessDeniedStartSupervisor;

    struct AccessDeniedThenActiveSupervisor {
        identities: Mutex<Vec<Option<WorkerIdentity>>>,
    }

    struct InstrumentedWorkerSupervisor {
        active: AtomicU64,
        max_active: AtomicU64,
        delay_ms: u64,
        wait_for_cancel: bool,
        canceled: AtomicBool,
        operation_id: Mutex<Option<OperationRef>>,
    }

    struct FailingCreateWorkerSupervisor;

    struct FailingStartSupervisor;

    struct FailingEvalSupervisor;

    struct BadEvalWriteSupervisor;

    struct FailingReverseOpenSupervisor;

    struct FailingReverseCloseSupervisor;

    struct FailingReverseCoreSupervisor;

    impl WorkerSupervisor for RecordingIdentitySupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            self.identities
                .lock()
                .unwrap()
                .push(request.identity.clone());
            Ok(WorkerHandle {
                worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str()))
                    .unwrap(),
                session_id: request.session_id,
                pipe_name: "test-pipe".to_string(),
                identity: request
                    .identity
                    .unwrap_or(WorkerIdentity::CurrentUserDevMode),
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
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for AccessDeniedStartSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Ok(test_worker_handle(request.session_id))
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::StartDebugSession { .. } => Ok(WorkerResponse::Failed {
                    code: "start_failed".to_string(),
                    message:
                        "native call failed with status 500: HRESULT 0x80070005: Access is denied."
                            .to_string(),
                    writes: Vec::new(),
                }),
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for AccessDeniedThenActiveSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            self.identities
                .lock()
                .unwrap()
                .push(request.identity.clone());
            let identity = request.identity.unwrap_or(WorkerIdentity::LocalSystem);
            Ok(WorkerHandle {
                worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str()))
                    .unwrap(),
                session_id: request.session_id,
                pipe_name: "test-pipe".to_string(),
                identity,
            })
        }

        fn request_worker(
            &self,
            worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::StartDebugSession { .. }
                    if worker.identity != WorkerIdentity::ActiveInteractiveUser =>
                {
                    Ok(WorkerResponse::Failed {
                        code: "start_failed".to_string(),
                        message:
                            "native call failed with status 500: HRESULT 0x80070005: Access is denied."
                                .to_string(),
                        writes: Vec::new(),
                    })
                }
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for FailingCreateWorkerSupervisor {
        fn create_worker(
            &self,
            _request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Err(ServiceError::Io(std::io::Error::from_raw_os_error(5)))
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            _request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            unreachable!("create_worker fails before worker requests")
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for FailingStartSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Ok(WorkerHandle {
                worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str()))
                    .unwrap(),
                session_id: request.session_id,
                pipe_name: "test-pipe".to_string(),
                identity: WorkerIdentity::CurrentUserDevMode,
            })
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::StartDebugSession { .. } => Ok(WorkerResponse::Failed {
                    code: "start_failed".to_string(),
                    message: "native open failed".to_string(),
                    writes: Vec::new(),
                }),
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for FailingEvalSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Ok(WorkerHandle {
                worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str()))
                    .unwrap(),
                session_id: request.session_id,
                pipe_name: "test-pipe".to_string(),
                identity: WorkerIdentity::CurrentUserDevMode,
            })
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::EvalDebugCommand { .. } => Ok(WorkerResponse::Failed {
                    code: "eval_failed".to_string(),
                    message: "mock eval failed".to_string(),
                    writes: Vec::new(),
                }),
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for BadEvalWriteSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Ok(test_worker_handle(request.session_id))
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::EvalDebugCommand {
                    session_id,
                    operation_id: _,
                    command,
                    ..
                } => Ok(WorkerResponse::DebugCommand {
                    result: DebugCommandResult {
                        session_id,
                        operation_id: None,
                        command,
                        output: "bad write".to_string(),
                        output_truncated: false,
                        full_output_byte_len: Some(9),
                        inline_output_byte_limit: Some(DEFAULT_INLINE_TEXT_BYTE_LIMIT as u64),
                        final_state: Some(DebugSessionState::Ready),
                        raw_output: None,
                        full_output_artifact_ref: None,
                        warnings: Vec::new(),
                        error: None,
                    },
                    writes: vec![WorkerArtifactWrite {
                        relative_path: PathBuf::from("outside.txt"),
                        kind: "debug.raw_output".to_string(),
                        byte_len: 9,
                        description: Some("invalid test write".to_string()),
                    }],
                }),
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for FailingReverseOpenSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Ok(test_worker_handle(request.session_id))
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::OpenReverseSession { .. } => Ok(WorkerResponse::Failed {
                    code: "reverse_open_failed".to_string(),
                    message: "open_database failed with result 4".to_string(),
                    writes: Vec::new(),
                }),
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for FailingReverseCloseSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Ok(test_worker_handle(request.session_id))
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::CloseReverseSession { .. } => Ok(WorkerResponse::Failed {
                    code: "reverse_close_failed".to_string(),
                    message: "mock IDA close failed".to_string(),
                    writes: Vec::new(),
                }),
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    impl WorkerSupervisor for FailingReverseCoreSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Ok(test_worker_handle(request.session_id))
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::ReverseCoreFunction { .. } => Ok(WorkerResponse::Failed {
                    code: "reverse_core_failed".to_string(),
                    message: "mock IDA core function failed".to_string(),
                    writes: Vec::new(),
                }),
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    fn test_worker_handle(session_id: SessionRef) -> WorkerHandle {
        WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", session_id.id.as_str())).unwrap(),
            session_id,
            pipe_name: "test-pipe".to_string(),
            identity: WorkerIdentity::CurrentUserDevMode,
        }
    }

    impl InstrumentedWorkerSupervisor {
        fn with_delay(delay_ms: u64) -> Self {
            Self {
                active: AtomicU64::new(0),
                max_active: AtomicU64::new(0),
                delay_ms,
                wait_for_cancel: false,
                canceled: AtomicBool::new(false),
                operation_id: Mutex::new(None),
            }
        }

        fn wait_for_cancel() -> Self {
            Self {
                wait_for_cancel: true,
                ..Self::with_delay(0)
            }
        }

        fn wait_for_operation_id(&self) -> Value {
            for _ in 0..100 {
                if let Some(operation_id) = self.operation_id.lock().unwrap().clone() {
                    return serde_json::to_value(operation_id).unwrap();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("operation id was not captured");
        }

        fn enter_request(&self) {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            loop {
                let current = self.max_active.load(Ordering::SeqCst);
                if active <= current {
                    break;
                }
                if self
                    .max_active
                    .compare_exchange(current, active, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    break;
                }
            }
        }

        fn leave_request(&self) {
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl WorkerSupervisor for InstrumentedWorkerSupervisor {
        fn create_worker(
            &self,
            request: WorkerCreateRequest,
        ) -> Result<WorkerHandle, ServiceError> {
            Ok(WorkerHandle {
                worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str()))
                    .unwrap(),
                session_id: request.session_id.clone(),
                pipe_name: format!(r"\\.\pipe\test-{}", request.session_id.id.as_str()),
                identity: WorkerIdentity::CurrentUserDevMode,
            })
        }

        fn request_worker(
            &self,
            _worker: &WorkerHandle,
            request: WorkerRequest,
        ) -> Result<WorkerResponse, ServiceError> {
            match request {
                WorkerRequest::EvalDebugCommand {
                    operation_id,
                    session_id,
                    command,
                    artifact_dir,
                } => {
                    *self.operation_id.lock().unwrap() = Some(operation_id.clone());
                    self.enter_request();
                    if self.wait_for_cancel {
                        for _ in 0..100 {
                            if self.canceled.load(Ordering::SeqCst) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    } else {
                        std::thread::sleep(Duration::from_millis(self.delay_ms));
                    }
                    let response =
                        write_mock_eval_response(session_id, operation_id, command, artifact_dir);
                    self.leave_request();
                    response
                }
                WorkerRequest::CloseSession { session_id } => {
                    self.enter_request();
                    std::thread::sleep(Duration::from_millis(self.delay_ms));
                    self.leave_request();
                    mock_worker_response(WorkerRequest::CloseSession { session_id })
                }
                other => mock_worker_response(other),
            }
        }

        fn cancel_worker_operation(
            &self,
            _worker: &WorkerHandle,
            _session_id: &SessionRef,
            _operation_id: &OperationRef,
        ) -> Result<WorkerCancelOutcome, ServiceError> {
            self.canceled.store(true, Ordering::SeqCst);
            Ok(WorkerCancelOutcome::Notified)
        }

        fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            Ok(())
        }

        fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
            self.canceled.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
}
