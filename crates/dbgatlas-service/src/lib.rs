use dbgatlas_debug::{
    AddSymbolsRequest, CreateDebugSession, DebugCommandResult, DebugMemoryResult,
    DebugSessionState, DebugTarget, EvalDebugCommand, ReadMemoryRequest,
};
use dbgatlas_model::{ArtifactRef, Id, OperationRef, RecordingRef, SessionRef, Timestamp};
use dbgatlas_recording::{RecordingPreset, RecordingState, RecordingTarget, StartRecording};
use dbgatlas_runtime::RuntimeConfig;
use dbgatlas_worker_protocol::{
    ReverseCoreFunctionResult, ReverseFunctionLookupResult, WorkerArtifactWrite, WorkerEnvelope,
    WorkerProtocolError, WorkerRequest, WorkerResponse, decode_jsonl, encode_jsonl,
};
use dbgatlas_workspace::{
    ArtifactMetadata, CommandAuditRecord, OperationRecord, OperationStatus, Workspace,
    WorkspaceError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub const INTERNAL_WORKSPACE_DIR: &str = "dbgatlas";
pub const DEFAULT_SERVICE_PORT: u16 = 7331;
pub const MAX_MEMORY_READ_LENGTH: u64 = 16 * 1024 * 1024;
pub const WINDOWS_SERVICE_NAME: &str = "DbgAtlas";
pub const WINDOWS_SERVICE_DISPLAY_NAME: &str = "DbgAtlas Service";
pub const WINDOWS_SERVICE_DESCRIPTION: &str = "DbgAtlas local debugging service";
pub const WINDOWS_SERVICE_DIR: &str = "DbgAtlas";
pub const WINDOWS_SERVICE_BIN_DIR: &str = "bin";
pub const WINDOWS_SERVICE_ETC_DIR: &str = "etc";
pub const WINDOWS_SERVICE_VAR_DIR: &str = "var";
pub const WINDOWS_SERVICE_LOG_DIR: &str = "log";
pub const WINDOWS_SERVICE_CONFIG_FILE: &str = "runtime.toml";
pub const WINDOWS_SERVICE_TOKEN_FILE: &str = "token";
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
pub const WINDOWS_SERVICE_OPTIONAL_PAYLOAD_FILES: &[&str] = &[
    "libgcc_s_seh-1.dll",
    "libstdc++-6.dll",
    "libwinpthread-1.dll",
];
pub const DEFAULT_IDA_INSTALL_DIR: &str = r"C:\Program Files\IDA Professional 9.3";

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
static RECORDING_COUNTER: AtomicU64 = AtomicU64::new(1);
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);
static ARTIFACT_COUNTER: AtomicU64 = AtomicU64::new(1);
static WORKER_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

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
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ServiceCapabilities {
    pub ida_py_eval: bool,
}

impl ServiceHost {
    pub fn new(supervisor: Arc<dyn WorkerSupervisor>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ServiceState::default())),
            supervisor,
            capabilities: ServiceCapabilities::default(),
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
            "reverse.imports" => self.reverse_core_function("imports", request.params),
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
            "reverse.force_recompile" => {
                self.reverse_core_function("force_recompile", request.params)
            }
            "reverse.idb_save" => self.reverse_core_function("idb_save", request.params),
            "reverse.py_eval" => self.reverse_py_eval(request.params),
            "reverse.find_bytes" => self.reverse_core_function("find_bytes", request.params),
            "reverse.search_text" => self.reverse_core_function("search_text", request.params),
            "reverse.xref_query" => self.reverse_core_function("xref_query", request.params),
            "reverse.func_query" => self.reverse_core_function("func_query", request.params),
            "reverse.entity_query" => self.reverse_core_function("entity_query", request.params),
            "reverse.session.close" => self.reverse_session_close(request.params),
            "recording.start" => self.recording_start(request.params),
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
        if request.id.is_none() {
            let _ = self.handle_mcp_method(request);
            return None;
        }
        let id = request.id.clone();
        Some(match self.handle_mcp_method(request) {
            Ok(result) => JsonRpcResponse::result(id, result),
            Err(error) => JsonRpcResponse::error(id, mcp_error_for(error)),
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
            "tools/list" => Ok(json!({ "tools": mcp_tool_descriptors(self.capabilities) })),
            "tools/call" => {
                let params: ToolCallParams =
                    serde_json::from_value(request.params.unwrap_or_else(|| json!({})))?;
                let result = self.call_mcp_tool_output(
                    &params.name,
                    params.arguments.unwrap_or_else(|| json!({})),
                )?;
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
            | "reverse.imports"
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
            | "reverse.force_recompile"
            | "reverse.idb_save"
            | "reverse.find_bytes"
            | "reverse.search_text"
            | "reverse.xref_query"
            | "reverse.func_query"
            | "reverse.entity_query"
            | "reverse.session.close" => self.call_mcp_service_tool(name, arguments),
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
            params: Some(arguments),
        });
        Ok(mcp_service_response_result(response))
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
        let workspace = Workspace::open(params.path)?;
        Ok(serde_json::to_value(workspace.facts()?)?)
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
        let params: DebugSessionCreateParams = parse_params(params)?;
        let target = params.target.validate()?;
        let request = CreateDebugSession {
            target: target.clone(),
            startup_timeout_ms: params.startup_timeout_ms,
        }
        .validate()?;
        let session_id = next_session_ref();
        let operation_id = next_operation_ref();
        let workspace = ensure_project_workspace(&params.project_root)?;
        let session_dir = workspace.ensure_session_artifact_dir(&session_id.id)?;
        let worker = self.supervisor.create_worker(WorkerCreateRequest {
            session_id: session_id.clone(),
            project_root: params.project_root.clone(),
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: session_dir.clone(),
            startup_timeout_ms: request.startup_timeout_ms.unwrap_or(5_000),
        })?;
        let start = self.supervisor.request_worker(
            &worker,
            WorkerRequest::StartDebugSession {
                session_id: session_id.clone(),
                target: request.target.clone(),
                artifact_dir: session_dir.clone(),
            },
        );
        let start_writes = match start {
            Ok(WorkerResponse::Ok { writes, .. }) => writes,
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let _ = self.supervisor.kill_worker(&worker);
                self.record_failed_session_create(
                    &workspace,
                    &operation_id,
                    &session_id,
                    format!("{code}: {message}"),
                )?;
                return Err(ServiceError::Worker(format!("{code}: {message}")));
            }
            Ok(other) => {
                let _ = self.supervisor.kill_worker(&worker);
                let message = format!("unexpected start response: {other:?}");
                self.record_failed_session_create(
                    &workspace,
                    &operation_id,
                    &session_id,
                    &message,
                )?;
                return Err(ServiceError::Worker(message));
            }
            Err(error) => {
                let _ = self.supervisor.kill_worker(&worker);
                self.record_failed_session_create(
                    &workspace,
                    &operation_id,
                    &session_id,
                    error.to_string(),
                )?;
                return Err(error);
            }
        };
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
                match self.supervisor.request_worker(
                    &session.worker,
                    WorkerRequest::CloseSession {
                        session_id: session.session_id.clone(),
                    },
                )? {
                    WorkerResponse::Ok { .. } => self.supervisor.close_worker(&session.worker)?,
                    WorkerResponse::Failed { code, message, .. } => {
                        return Err(ServiceError::Worker(format!("{code}: {message}")));
                    }
                    other => {
                        return Err(ServiceError::Worker(format!(
                            "unexpected close response: {other:?}"
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

    fn debug_eval(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugEvalParams = parse_params(params)?;
        let request = EvalDebugCommand {
            session_id: params.session_id.clone(),
            command: params.command,
            timeout_ms: params.timeout_ms,
        };
        request.validate()?;
        self.eval_command(request, "debug.eval")
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
        let worker = self.supervisor.create_worker(WorkerCreateRequest {
            session_id: session_id.clone(),
            project_root: params.project_root.clone(),
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: artifact_dir.clone(),
            startup_timeout_ms: 5_000,
        })?;

        let open = self.supervisor.request_worker(
            &worker,
            WorkerRequest::OpenReverseSession {
                session_id: session_id.clone(),
                ida_install_dir: ida_install_dir.clone(),
                database_path: params.database_path.clone(),
                artifact_dir: artifact_dir.clone(),
            },
        );
        match open {
            Ok(WorkerResponse::ReverseSessionOpened { .. }) => {}
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let _ = self.supervisor.kill_worker(&worker);
                let error = worker_failed_message(code, message);
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
                let error = format!("unexpected reverse open response: {other:?}");
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
                let message = error.to_string();
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
                "writes_idb": false
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
                    operation_id: operation_id.clone(),
                    runtime_address,
                    runtime_module_base,
                    ida_image_base,
                    artifact_dir: session.artifact_dir.clone(),
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
                    operation_id: operation_id.clone(),
                    function: function.to_string(),
                    arguments: arguments.clone(),
                    artifact_dir: session.artifact_dir.clone(),
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

        let worker_response = self.supervisor.request_worker(
            &session.worker,
            WorkerRequest::EvalDebugCommand {
                session_id: session.session_id.clone(),
                operation_id: operation_id.clone(),
                command: request.command.clone(),
                artifact_dir: session.artifact_dir.clone(),
            },
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
                result.raw_output = registered.raw_output.clone();
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
                    registered.artifacts,
                    registered.raw_output,
                )?;
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
                    status,
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
                Err(error)
            }
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
                    registered.artifacts,
                    registered.raw_output,
                )?;
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
                    status,
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
                Err(error)
            }
        }
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, ServiceState>, ServiceError> {
        self.state
            .lock()
            .map_err(|_| ServiceError::Rpc("service state lock poisoned".to_string()))
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
        Self {
            ida_py_eval: runtime.tools.ida.allow_py_eval,
        }
    }
}

#[derive(Default)]
struct ServiceState {
    sessions: HashMap<String, ManagedSession>,
    recordings: HashMap<String, ManagedRecording>,
    operations: HashMap<String, ServiceOperation>,
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
            identity: self.identity.clone(),
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
            identity: WorkerIdentity::ActiveInteractiveUser,
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
        let worker_id = Id::new(format!("worker-{}", request.session_id.id.as_str()))
            .expect("generated worker ids are valid");
        let pipe_name = unique_pipe_name(&request.session_id);
        let transport = WorkerTransport::create_server(&pipe_name)?;
        let worker_exe = self
            .worker_exe
            .clone()
            .map(Ok)
            .unwrap_or_else(worker_executable_path)?;
        let mut child = spawn_worker_process(
            &worker_exe,
            &pipe_name,
            request.session_id.id.as_str(),
            &self.identity,
        )?;
        self.job.assign_process(&child)?;
        let connected = match transport.connect(request.startup_timeout_ms) {
            Ok(connected) => connected,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
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
        Ok(WorkerHandle {
            worker_id,
            pipe_name,
            session_id: request.session_id,
            identity: self.identity.clone(),
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
        transport.request(request)
    }

    fn cancel_worker_operation(
        &self,
        worker: &WorkerHandle,
        session_id: &SessionRef,
        operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        let worker_state = self.get_worker(worker)?;
        if let Ok(mut transport) = worker_state.transport.try_lock() {
            let _ = transport.request(WorkerRequest::CancelOperation {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
            })?;
            return Ok(WorkerCancelOutcome::Notified);
        }
        self.kill_worker(worker)?;
        Ok(WorkerCancelOutcome::WorkerKilled)
    }

    fn close_worker(&self, worker: &WorkerHandle) -> Result<(), ServiceError> {
        let worker_state = self
            .workers
            .lock()
            .map_err(|_| ServiceError::Worker("worker registry lock poisoned".to_string()))?
            .remove(worker.worker_id.as_str());
        if let Some(worker_state) = worker_state {
            let mut child = worker_state
                .child
                .lock()
                .map_err(|_| ServiceError::Worker("worker process lock poisoned".to_string()))?;
            let _ = child.wait();
        }
        Ok(())
    }

    fn kill_worker(&self, worker: &WorkerHandle) -> Result<(), ServiceError> {
        let worker_state = self
            .workers
            .lock()
            .map_err(|_| ServiceError::Worker("worker registry lock poisoned".to_string()))?
            .remove(worker.worker_id.as_str());
        if let Some(worker_state) = worker_state {
            let mut child = worker_state
                .child
                .lock()
                .map_err(|_| ServiceError::Worker("worker process lock poisoned".to_string()))?;
            let _ = child.kill();
            let _ = child.wait();
        }
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
        "result": core.result,
        "warnings": core.warnings,
        "operation": {
            "status": "success",
            "artifact_refs": [artifact_id],
            "raw_output_ref": null
        }
    }))
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
        "imports" => mock_imports(&arguments)?,
        "list_strings" => mock_list_strings(&arguments)?,
        "get_string" => mock_get_string(&arguments)?,
        "get_bytes" => mock_get_bytes(&arguments)?,
        "get_int" => mock_get_int(&arguments)?,
        "decompile" => mock_decompile(&arguments)?,
        "disasm" => mock_disasm(&arguments)?,
        "xrefs_to" => mock_xrefs_to(&arguments)?,
        "xrefs_to_field" => mock_xrefs_to_field(&arguments)?,
        "callees" => mock_callees(&arguments)?,
        "rename" => mock_batch_write_result(&arguments, "items"),
        "set_comments" => mock_batch_write_result(&arguments, "items"),
        "set_type" => mock_batch_write_result(&arguments, "items"),
        "declare_type" => mock_declare_type(&arguments),
        "force_recompile" => mock_force_recompile(&arguments),
        "idb_save" => mock_idb_save(&arguments),
        "py_eval" => mock_py_eval(&arguments),
        "find_bytes" => mock_find_bytes(&arguments)?,
        "search_text" => mock_search_text(&arguments)?,
        "xref_query" => mock_xref_query(&arguments)?,
        "func_query" => mock_func_query(&arguments)?,
        "entity_query" => mock_entity_query(&arguments)?,
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

fn mock_imports(arguments: &Value) -> Result<Value, ServiceError> {
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

fn mock_declare_type(arguments: &Value) -> Value {
    let decls = normalize_core_list(arguments.get("decls").unwrap_or(&Value::Null));
    json!({
        "ok": true,
        "count": decls.len(),
        "changed_count": decls.len(),
        "errors": 0
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

fn mock_xref_query(arguments: &Value) -> Result<Value, ServiceError> {
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

fn mock_func_query(arguments: &Value) -> Result<Value, ServiceError> {
    mock_list_funcs(arguments)
}

fn mock_entity_query(arguments: &Value) -> Result<Value, ServiceError> {
    match arguments
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("functions")
    {
        "functions" => mock_list_funcs(arguments),
        "globals" | "names" => mock_list_globals(arguments),
        "imports" => mock_imports(arguments),
        "strings" => mock_list_strings(arguments),
        other => Err(ServiceError::Rpc(format!(
            "unsupported entity_query kind `{other}`"
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
    let output = format!(
        "mock debug worker accepted eval command; real DbgEng execution is not wired yet\ncommand: {}\n",
        command
    );
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

    Ok(WorkerResponse::DebugCommand {
        result: DebugCommandResult {
            session_id,
            operation_id: None,
            command,
            output: output.clone(),
            final_state: Some(DebugSessionState::Ready),
            raw_output: None,
            warnings: vec!["mock worker: real DbgEng execution is not wired yet".to_string()],
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
    memory: Option<ArtifactRef>,
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
    let mut memory = None;
    for write in writes {
        let artifact_id = next_artifact_ref();
        if write.kind == "debug.raw_output" {
            raw_output = Some(artifact_id.clone());
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

fn spawn_worker_process(
    worker_exe: &Path,
    pipe_name: &str,
    session_id: &str,
    identity: &WorkerIdentity,
) -> Result<WorkerProcess, ServiceError> {
    match identity {
        WorkerIdentity::ActiveInteractiveUser => {
            spawn_active_interactive_worker_process(worker_exe, pipe_name, session_id)
        }
        WorkerIdentity::CurrentUserDevMode | WorkerIdentity::LocalSystem => Ok(WorkerProcess::Std(
            Command::new(worker_exe)
                .arg("--pipe")
                .arg(pipe_name)
                .arg("--session-id")
                .arg(session_id)
                .spawn()?,
        )),
    }
}

#[cfg(windows)]
fn spawn_active_interactive_worker_process(
    worker_exe: &Path,
    pipe_name: &str,
    session_id: &str,
) -> Result<WorkerProcess, ServiceError> {
    windows_active_user_process::spawn(
        worker_exe,
        &["--pipe", pipe_name, "--session-id", session_id],
    )
    .map(WorkerProcess::RawWindows)
}

#[cfg(not(windows))]
fn spawn_active_interactive_worker_process(
    _worker_exe: &Path,
    _pipe_name: &str,
    _session_id: &str,
) -> Result<WorkerProcess, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceInstallPaths {
    pub root_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub staging_bin_dir: PathBuf,
    pub etc_dir: PathBuf,
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
        let var_dir = root_dir.join(WINDOWS_SERVICE_VAR_DIR);
        let log_dir = var_dir.join(WINDOWS_SERVICE_LOG_DIR);
        Self {
            staging_bin_dir: root_dir.join("bin.staging"),
            config_path: etc_dir.join(WINDOWS_SERVICE_CONFIG_FILE),
            token_file: etc_dir.join(WINDOWS_SERVICE_TOKEN_FILE),
            installed_exe: bin_dir.join("dbgatlas.exe"),
            bin_dir,
            etc_dir,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServicePayloadFile {
    pub file_name: String,
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Clone, Debug)]
pub struct WindowsServiceInstallOptions {
    pub bind: SocketAddr,
    pub force: bool,
}

impl Default for WindowsServiceInstallOptions {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_SERVICE_PORT),
            force: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WindowsServiceUninstallOptions {
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
    pub source_dir: PathBuf,
    pub restart: bool,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub struct WindowsServiceRunOptions {
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
    payload: Vec<ServicePayloadFile>,
    response: WindowsServiceUpdateAccepted,
}

pub fn default_windows_service_paths() -> ServiceInstallPaths {
    let root = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join(WINDOWS_SERVICE_DIR);
    ServiceInstallPaths::for_root(root)
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
    for file_name in WINDOWS_SERVICE_OPTIONAL_PAYLOAD_FILES {
        let source = source_dir.join(file_name);
        if !source.is_file() {
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
    installed_client_config_from_paths(&default_windows_service_paths())
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

pub fn start_windows_service() -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::start()
}

pub fn stop_windows_service() -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::stop()
}

pub fn status_windows_service() -> Result<WindowsServiceCommandResult, ServiceError> {
    windows_service_control::status()
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
    let config = format!("version = 1\n\n[server]\nbind = \"{}\"\n", bind);
    let runtime = RuntimeConfig::from_toml_str(&config)?;
    fs::write(&paths.config_path, config)?;
    Ok(runtime)
}

fn prepare_install_layout(paths: &ServiceInstallPaths) -> Result<(), ServiceError> {
    fs::create_dir_all(&paths.root_dir)?;
    fs::create_dir_all(&paths.etc_dir)?;
    fs::create_dir_all(&paths.log_dir)?;
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
    let payload = discover_service_payload(&source_dir, &paths.bin_dir)?;
    let response = WindowsServiceUpdateAccepted {
        status: "accepted".to_string(),
        source_dir: source_dir.clone(),
        service_name: WINDOWS_SERVICE_NAME.to_string(),
        installed_binary: paths.installed_exe.clone(),
        log_dir: paths.log_dir.clone(),
        payload: payload.clone(),
        restart,
    };
    Ok(PreparedServiceUpdate {
        source_dir,
        payload,
        response,
    })
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

    pub fn start() -> Result<WindowsServiceCommandResult, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn stop() -> Result<WindowsServiceCommandResult, ServiceError> {
        Err(ServiceError::ServiceControlUnsupported)
    }

    pub fn status() -> Result<WindowsServiceCommandResult, ServiceError> {
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
        let paths = default_windows_service_paths();
        let current_exe = std::env::current_exe()?;
        let source_dir = current_exe.parent().ok_or_else(|| {
            ServiceError::ServiceControl("current executable has no parent".into())
        })?;
        if source_is_installed_bin(source_dir, &paths) {
            return Err(ServiceError::ServiceControl(
                "cannot install from the installed bin directory; run install from a development or release payload directory".to_string(),
            ));
        }
        let payload = discover_service_payload(source_dir, &paths.bin_dir)?;

        let manager =
            manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        if let Some(service) = open_optional(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::CHANGE_CONFIG,
        )? {
            let status = service.query_status().map_err(map_windows_service_error)?;
            if status.current_state != ServiceState::Stopped {
                return Err(ServiceError::ServiceIsRunning);
            }
            if !options.force {
                return Err(ServiceError::ServiceControl(
                    "service is already installed; use `dbgatlas service install --force` to update payload and service entry".to_string(),
                ));
            }
        }

        prepare_install_layout(&paths)?;
        let runtime = create_runtime_config_if_missing(&paths, options.bind)?;
        ensure_token_file(&paths.token_file)?;
        install_payload(&payload, &paths)?;
        create_or_update_service(&manager, &paths)?;

        Ok(result(
            "installed",
            Some(runtime.server.bind),
            paths,
            payload,
        ))
    }

    pub fn start() -> Result<WindowsServiceCommandResult, ServiceError> {
        let paths = default_windows_service_paths();
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

    pub fn stop() -> Result<WindowsServiceCommandResult, ServiceError> {
        let paths = default_windows_service_paths();
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

    pub fn status() -> Result<WindowsServiceCommandResult, ServiceError> {
        let paths = default_windows_service_paths();
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
        let paths = default_windows_service_paths();
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let Some(service) = open_optional(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )?
        else {
            cleanup_install_dirs(&paths, options.purge)?;
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
            "accepted service.update from {}; updater pid={}",
            prepared.source_dir.display(),
            child.id()
        ));
        Ok(prepared.response)
    }

    pub fn apply_update(
        options: WindowsServiceApplyUpdateOptions,
    ) -> Result<WindowsServiceCommandResult, ServiceError> {
        let timeout = validate_update_timeout(options.timeout_ms)?;
        let deadline = Instant::now() + timeout;
        let paths = default_windows_service_paths();
        prepare_install_layout(&paths)?;
        let prepared = prepare_service_update(&options.source_dir, &paths, options.restart)?;
        let suffix = update_dir_suffix();
        let staging_dir = paths.root_dir.join(format!("bin.next-{suffix}"));
        append_service_log(&format!(
            "starting service apply-update from {}; staging {}",
            prepared.source_dir.display(),
            staging_dir.display()
        ));
        copy_update_payload_to_staging(&prepared.payload, &staging_dir)?;

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

        if options.restart {
            start_service_with_timeout(&service, remaining_update_timeout(deadline)?)?;
            append_service_log("service restarted after apply-update");
        } else {
            append_service_log("service restart skipped after apply-update");
        }

        if let Err(error) = cleanup_update_dirs(&paths) {
            append_service_log(&format!("service update cleanup failed: {error}"));
        }

        Ok(result(
            if options.restart {
                "running"
            } else {
                "stopped"
            },
            installed_endpoint(&paths).ok().flatten(),
            paths,
            prepared.payload,
        ))
    }

    pub fn run_dispatcher(options: WindowsServiceRunOptions) -> Result<(), ServiceError> {
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
            ServiceHost::with_installed_process_workers()?
                .with_capabilities(ServiceCapabilities::from_runtime_config(&runtime)),
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

    fn cleanup_install_dirs(paths: &ServiceInstallPaths, purge: bool) -> Result<(), ServiceError> {
        if paths.bin_dir.exists() {
            fs::remove_dir_all(&paths.bin_dir)?;
        }
        if paths.staging_bin_dir.exists() {
            fs::remove_dir_all(&paths.staging_bin_dir)?;
        }
        if purge && paths.root_dir.exists() {
            fs::remove_dir_all(&paths.root_dir)?;
        }
        Ok(())
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

    fn request(&mut self, request: WorkerRequest) -> Result<WorkerResponse, ServiceError> {
        let request_id = next_worker_request_id();
        let envelope = WorkerEnvelope::new(request_id.clone(), request);
        let line = encode_jsonl(&envelope)?;
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        let response_line = read_jsonl_line(&mut self.file)?;
        let response: WorkerEnvelope<WorkerResponse> = decode_jsonl(&response_line)?;
        if response.request_id != request_id {
            return Err(ServiceError::Worker(format!(
                "worker response id mismatch: expected {request_id}, got {}",
                response.request_id
            )));
        }
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
        if bytes.len() > 1024 * 1024 {
            return Err(ServiceError::Worker(
                "worker response line is too large".to_string(),
            ));
        }
    }
    String::from_utf8(bytes).map_err(|error| ServiceError::Worker(error.to_string()))
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
}

impl ToolCallOutput {
    fn success(value: Value) -> Self {
        Self {
            value,
            is_error: false,
        }
    }

    fn error(value: Value) -> Self {
        Self {
            value,
            is_error: true,
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
            "Create a debug session from a dump or attach target.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "target": { "type": "object" }
                },
                "required": ["project_root", "target"]
            }),
        ),
        mcp_tool(
            "debug.eval",
            "Execute a raw WinDbg command in an existing session.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "object" },
                    "command": { "type": "string" }
                },
                "required": ["session_id", "command"]
            }),
        ),
        mcp_tool(
            "debug.modules",
            "List modules for a debug session.",
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
            "Add a symbol path to a debug session.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "object" },
                    "symbol_path": { "type": "string" },
                    "reload": { "type": "boolean" }
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
                    "session_id": { "type": "object" },
                    "address": {},
                    "length": { "type": "integer" }
                },
                "required": ["session_id", "address", "length"]
            }),
        ),
        mcp_tool(
            "reverse.session.open",
            "Open an IDA reverse session.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "database_path": { "type": "string" },
                    "ida_install_dir": { "type": "string" }
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
                    "session_id": { "type": "object" },
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
            "reverse.imports",
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
            "Rename IDA functions or globals.",
            mcp_reverse_core_schema_required(json!({ "items": {} }), &["items"]),
        ),
        mcp_tool(
            "reverse.set_comments",
            "Set IDA comments at addresses.",
            mcp_reverse_core_schema_required(json!({ "items": {} }), &["items"]),
        ),
        mcp_tool(
            "reverse.set_type",
            "Apply C types to IDA functions, globals, or addresses.",
            mcp_reverse_core_schema_required(json!({ "items": {} }), &["items"]),
        ),
        mcp_tool(
            "reverse.declare_type",
            "Declare C types in the IDA local type library.",
            mcp_reverse_core_schema_required(json!({ "decls": {} }), &["decls"]),
        ),
        mcp_tool(
            "reverse.force_recompile",
            "Invalidate Hex-Rays cached decompilation for functions or all functions.",
            mcp_reverse_core_schema(json!({ "addrs": {} })),
        ),
        mcp_tool(
            "reverse.idb_save",
            "Save the current IDA database.",
            mcp_reverse_core_schema(json!({ "path": { "type": "string" } })),
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
            "reverse.xref_query",
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
            "reverse.func_query",
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
            "reverse.entity_query",
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
                    "session_id": { "type": "object" }
                },
                "required": ["session_id"]
            }),
        ),
        mcp_tool(
            "debug.session.close",
            "Close a debug session.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.session.kill",
            "Kill a debug session worker.",
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

fn mcp_session_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session_id": { "type": "object" }
        },
        "required": ["session_id"]
    })
}

fn mcp_reverse_core_schema(extra_properties: Value) -> Value {
    mcp_reverse_core_schema_required(extra_properties, &[])
}

fn mcp_reverse_core_schema_required(extra_properties: Value, extra_required: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("session_id".to_string(), json!({ "type": "object" }));
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
            "operation_id": { "type": "object" }
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
    while !shutdown.is_stopping() {
        let (mut stream, _) = match listener.accept() {
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
            let response = match handle_http_stream(&mut stream, &config, &host) {
                Ok(response) => response,
                Err(error) => match http_json_response(
                    http_status_for(&error),
                    &JsonRpcResponse::error(None, rpc_error_for(error)),
                ) {
                    Ok(response) => response,
                    Err(error) => {
                        let _ = stream.write_all(error.to_string().as_bytes());
                        return;
                    }
                },
            };
            let _ = stream.write_all(response.as_bytes());
        });
    }
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
) -> Result<String, ServiceError> {
    let request = read_http_request(stream)?;
    authorize_http_request(&request, config)?;
    if request.method != "POST" {
        return Err(ServiceError::UnsupportedHttpMethod(request.method));
    }
    let rpc: JsonRpcRequest = serde_json::from_slice(&request.body)?;
    if rpc.jsonrpc != "2.0" {
        return Err(ServiceError::Rpc("jsonrpc must be `2.0`".to_string()));
    }
    match request.path.as_str() {
        "/rpc" => http_json_response(200, &host.handle_rpc(rpc)),
        "/mcp" => match host.handle_mcp(rpc) {
            Some(response) => http_json_response(200, &response),
            None => Ok(http_empty_response(202, "Accepted")),
        },
        other => Err(ServiceError::InvalidHttpRequest(format!(
            "unsupported path `{other}`"
        ))),
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
    }
}

#[cfg(windows)]
mod windows_active_user_process {
    use super::{ServiceError, WINDOWS_SERVICE_NAME};
    use std::ffi::{OsStr, c_void};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE, WAIT_FAILED};
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, SecurityImpersonation, TOKEN_ALL_ACCESS, TokenPrimary,
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
        waited: bool,
    }

    unsafe impl Send for RawProcess {}

    impl RawProcess {
        pub fn handle(&self) -> HANDLE {
            self.process_handle
        }

        pub fn kill(&mut self) -> Result<(), io::Error> {
            if self.process_handle.is_null() || self.waited {
                return Ok(());
            }
            let mut exit_code = 0;
            let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
            if ok != 0 && exit_code != STILL_ACTIVE as u32 {
                return Ok(());
            }
            let ok = unsafe { TerminateProcess(self.process_handle, 1) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn wait(&mut self) -> Result<(), io::Error> {
            if self.process_handle.is_null() || self.waited {
                return Ok(());
            }
            let status = unsafe { WaitForSingleObject(self.process_handle, INFINITE) };
            if status == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            self.waited = true;
            Ok(())
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

    pub fn spawn(worker_exe: &Path, args: &[&str]) -> Result<RawProcess, ServiceError> {
        let session_id = unsafe { WTSGetActiveConsoleSessionId() };
        if session_id == u32::MAX {
            return Err(ServiceError::Worker(
                "no active interactive session is available for IDA worker".to_string(),
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

        let mut primary_token = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateTokenEx(
                impersonation_token.raw(),
                TOKEN_ALL_ACCESS,
                std::ptr::null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut primary_token,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "DuplicateTokenEx for active interactive session {session_id} failed: {}",
                io::Error::last_os_error()
            )));
        }
        let primary_token = Handle::new(primary_token);
        let environment = EnvironmentBlock::create(primary_token.raw())?;

        let mut command_line = command_line(worker_exe.as_os_str(), args);
        let mut desktop = wide_null("winsta0\\default");
        let current_directory = worker_exe
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
                "CreateProcessAsUserW failed to launch {WINDOWS_SERVICE_NAME} worker in active interactive session {session_id}: {}",
                io::Error::last_os_error()
            )));
        }

        Ok(RawProcess {
            process_handle: process_info.hProcess,
            thread_handle: process_info.hThread,
            waited: false,
        })
    }

    fn command_line(executable: &OsStr, args: &[&str]) -> Vec<u16> {
        let mut parts = Vec::with_capacity(args.len() + 1);
        parts.push(quote_arg(&executable.to_string_lossy()));
        parts.extend(args.iter().map(|arg| quote_arg(arg)));
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
        let root = PathBuf::from(r"C:\ProgramData\DbgAtlas");
        let paths = ServiceInstallPaths::for_root(root.clone());

        assert_eq!(paths.root_dir, root);
        assert_eq!(paths.bin_dir, PathBuf::from(r"C:\ProgramData\DbgAtlas\bin"));
        assert_eq!(paths.etc_dir, PathBuf::from(r"C:\ProgramData\DbgAtlas\etc"));
        assert_eq!(paths.var_dir, PathBuf::from(r"C:\ProgramData\DbgAtlas\var"));
        assert_eq!(
            paths.log_dir,
            PathBuf::from(r"C:\ProgramData\DbgAtlas\var\log")
        );
        assert_eq!(
            paths.installed_exe,
            PathBuf::from(r"C:\ProgramData\DbgAtlas\bin\dbgatlas.exe")
        );
        assert_eq!(
            paths.config_path,
            PathBuf::from(r"C:\ProgramData\DbgAtlas\etc\runtime.toml")
        );
        assert_eq!(
            paths.token_file,
            PathBuf::from(r"C:\ProgramData\DbgAtlas\etc\token")
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
        for file_name in WINDOWS_SERVICE_OPTIONAL_PAYLOAD_FILES {
            fs::write(temp.path().join(file_name), "").unwrap();
        }

        let payload = discover_service_payload(temp.path(), &destination).unwrap();

        assert_eq!(
            payload.len(),
            WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len()
                + WINDOWS_SERVICE_OPTIONAL_PAYLOAD_FILES.len()
        );
        assert!(payload.iter().any(|file| {
            file.file_name == "dbgatlas-worker.exe"
                && file.destination == destination.join("dbgatlas-worker.exe")
        }));
        assert!(payload.iter().any(|file| {
            file.file_name == "dbgatlas_dbgeng.dll"
                && file.destination == destination.join("dbgatlas_dbgeng.dll")
        }));
        assert!(payload.iter().any(|file| {
            file.file_name == "libstdc++-6.dll"
                && file.destination == destination.join("libstdc++-6.dll")
        }));
    }

    #[test]
    fn payload_discovery_accepts_payload_without_optional_runtime_files() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("install-bin");
        for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
            fs::write(temp.path().join(file_name), "").unwrap();
        }

        let payload = discover_service_payload(temp.path(), &destination).unwrap();

        assert_eq!(payload.len(), WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len());
        assert!(
            !payload
                .iter()
                .any(|file| file.file_name == "libstdc++-6.dll")
        );
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
        assert_eq!(installed.identity, WorkerIdentity::ActiveInteractiveUser);
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
            ("reverse.imports", json!({ "offset": 1, "count": 1 })),
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
                "reverse.xref_query",
                json!({ "target": "0x140001000", "direction": "to", "xref_type": "all" }),
            ),
            (
                "reverse.func_query",
                json!({ "filter": "parse", "sort_by": "name" }),
            ),
            (
                "reverse.entity_query",
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
            "reverse.imports",
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
            "reverse.force_recompile",
            "reverse.idb_save",
            "reverse.find_bytes",
            "reverse.search_text",
            "reverse.py_eval",
            "reverse.xref_query",
            "reverse.func_query",
            "reverse.entity_query",
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
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.kind == "reverse.adapter_error")
        );
        let operations = workspace.list_operations().unwrap();
        let failed = operations
            .iter()
            .find(|operation| operation.capability == "reverse.session.open")
            .unwrap();
        assert!(matches!(failed.status, OperationStatus::Failed));
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
            "reverse.imports",
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
            "reverse.force_recompile",
            "reverse.idb_save",
            "reverse.find_bytes",
            "reverse.search_text",
            "reverse.xref_query",
            "reverse.func_query",
            "reverse.entity_query",
        ] {
            assert!(
                tools.iter().any(|tool| tool["name"] == tool_name),
                "{tool_name} tool is listed"
            );
        }
        assert!(tools.iter().any(|tool| tool["name"] == "workspace.facts"));
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
                "target": { "kind": "dump", "path": "sample.dmp" }
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
                "target": { "kind": "dump", "path": "sample.dmp" }
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
    fn http_mcp_workspace_facts_reads_recording_layer() {
        let temp = tempfile::tempdir().unwrap();
        let server = start_mock_http_service();
        let create = mcp_tool_call(
            server.endpoint,
            "debug.session.create",
            json!({
                "project_root": temp.path(),
                "target": { "kind": "dump", "path": "sample.dmp" }
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

    fn mcp_tool_call(endpoint: SocketAddr, name: &str, arguments: Value) -> Value {
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
        let body = http_body_json(&response);
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
                "target": { "kind": "dump", "path": "sample.dmp" }
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

    struct InstrumentedWorkerSupervisor {
        active: AtomicU64,
        max_active: AtomicU64,
        delay_ms: u64,
        wait_for_cancel: bool,
        canceled: AtomicBool,
        operation_id: Mutex<Option<OperationRef>>,
    }

    struct FailingStartSupervisor;

    struct FailingEvalSupervisor;

    struct FailingReverseOpenSupervisor;

    struct FailingReverseCloseSupervisor;

    struct FailingReverseCoreSupervisor;

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
                    message: "mock IDA open failed".to_string(),
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
