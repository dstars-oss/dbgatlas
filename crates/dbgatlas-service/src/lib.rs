use dbgatlas_debug::{
    CreateDebugSession, DebugCommandResult, DebugSessionState, DebugTarget, EvalDebugCommand,
};
use dbgatlas_model::{ArtifactRef, Id, OperationRef, SessionRef, Timestamp};
use dbgatlas_workspace::{
    ArtifactMetadata, OperationRecord, OperationStatus, Workspace, WorkspaceError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;

pub const INTERNAL_WORKSPACE_DIR: &str = "dbgatlas";
pub const DEFAULT_SERVICE_PORT: u16 = 7331;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);
static ARTIFACT_COUNTER: AtomicU64 = AtomicU64::new(1);

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
    #[error("operation not found: {0}")]
    OperationNotFound(String),
    #[error("operation is not running and cannot be canceled: {0}")]
    OperationNotCancelable(String),
    #[error(transparent)]
    Debug(#[from] dbgatlas_debug::DebugError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct ServiceHost {
    state: Arc<Mutex<ServiceState>>,
    supervisor: Arc<dyn WorkerSupervisor>,
}

impl ServiceHost {
    pub fn new(supervisor: Arc<dyn WorkerSupervisor>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ServiceState::default())),
            supervisor,
        }
    }

    pub fn with_mock_workers() -> Self {
        Self::new(Arc::new(MockWorkerSupervisor::new()))
    }

    pub fn handle_rpc(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();
        let result = match request.method.as_str() {
            "service.health" => self.service_health(),
            "service.info" => self.service_info(),
            "operation.get" => self.operation_get(request.params),
            "operation.cancel" => self.operation_cancel(request.params),
            "operation.stream" => self.operation_stream(request.params),
            "debug.session.create" => self.debug_session_create(request.params),
            "debug.session.close" => self.debug_session_close(request.params),
            "debug.session.kill" => self.debug_session_kill(request.params),
            "debug.eval" => self.debug_eval(request.params),
            "debug.modules" => self.debug_builtin_eval(request.params, "lm"),
            "debug.threads" => self.debug_builtin_eval(request.params, "~"),
            "debug.stack" => self.debug_builtin_eval(request.params, "k"),
            other => Err(ServiceError::Rpc(format!("unknown method `{other}`"))),
        };

        match result {
            Ok(value) => JsonRpcResponse::result(id, value),
            Err(error) => JsonRpcResponse::error(id, rpc_error_for(error)),
        }
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
        operation.updated_at = Timestamp::now();
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
        let worker = self.supervisor.create_debug_worker(WorkerCreateRequest {
            session_id: session_id.clone(),
            project_root: params.project_root.clone(),
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: session_dir.clone(),
            target: request.target.clone(),
        })?;
        let now = Timestamp::now();

        let session = ManagedSession {
            session_id: session_id.clone(),
            capability: "debug".to_string(),
            project_root: params.project_root,
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: session_dir,
            target,
            state: DebugSessionState::Ready,
            worker,
            created_at: now,
            updated_at: now,
            last_operation: Some(operation_id.clone()),
        };

        let operation = ServiceOperation::success(
            operation_id.clone(),
            "debug.session.create",
            Some(session_id.clone()),
            "debug session created",
        );
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "debug.session.create".to_string(),
            status: OperationStatus::Success,
            created_at: now,
            summary: "debug session created".to_string(),
            artifacts: Vec::new(),
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
        let operation_id = next_operation_ref();
        match mode {
            SessionFinishMode::Close => self.supervisor.close_worker(&session.worker)?,
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
        self.eval_command(request)
    }

    fn debug_builtin_eval(
        &self,
        params: Option<Value>,
        command: &'static str,
    ) -> Result<Value, ServiceError> {
        let params: SessionParams = parse_params(params)?;
        let request = EvalDebugCommand {
            session_id: params.session_id,
            command: command.to_string(),
            timeout_ms: None,
        };
        self.eval_command(request)
    }

    fn eval_command(&self, request: EvalDebugCommand) -> Result<Value, ServiceError> {
        let mut session = {
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
        let raw_relative_path = session_relative_path(
            &session.session_id,
            &format!("raw/{}.txt", operation_id.id.as_str()),
        );
        let transcript_relative_path = session_relative_path(&session.session_id, "transcript.log");
        let output = format!(
            "mock debug worker accepted eval command; real DbgEng execution is not wired yet\ncommand: {}\n",
            request.command
        );
        workspace.append_text_artifact(&raw_relative_path, &output)?;
        workspace.append_text_artifact(
            &transcript_relative_path,
            &format!("> {}\n{}\n", request.command, output),
        )?;

        let raw_artifact_id = next_artifact_ref();
        let transcript_artifact_id = next_artifact_ref();
        workspace.register_artifact(&ArtifactMetadata {
            artifact_id: raw_artifact_id.clone(),
            kind: "debug.raw_output".to_string(),
            relative_path: raw_relative_path.clone(),
            created_at: Timestamp::now(),
            operation_id: Some(operation_id.clone()),
            description: Some("mock debug eval raw output".to_string()),
        })?;
        workspace.register_artifact(&ArtifactMetadata {
            artifact_id: transcript_artifact_id.clone(),
            kind: "debug.transcript".to_string(),
            relative_path: transcript_relative_path,
            created_at: Timestamp::now(),
            operation_id: Some(operation_id.clone()),
            description: Some("debug session transcript".to_string()),
        })?;
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "debug.eval".to_string(),
            status: OperationStatus::Success,
            created_at: Timestamp::now(),
            summary: "debug eval completed by mock worker".to_string(),
            artifacts: vec![raw_artifact_id.clone(), transcript_artifact_id.clone()],
        })?;

        session.last_operation = Some(operation_id.clone());
        session.updated_at = Timestamp::now();
        let result = DebugCommandResult {
            session_id: session.session_id.clone(),
            operation_id: Some(operation_id.clone()),
            command: request.command,
            output,
            final_state: Some(session.state),
            raw_output: Some(raw_artifact_id.clone()),
            warnings: vec!["mock worker: real DbgEng execution is not wired yet".to_string()],
            error: None,
        };
        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "debug.eval",
            Some(session.session_id.clone()),
            "debug eval completed",
        );
        operation.artifacts.push(raw_artifact_id);
        operation.artifacts.push(transcript_artifact_id);
        operation.events.push(ServiceEvent {
            timestamp: Timestamp::now(),
            kind: "output".to_string(),
            message: result.output.clone(),
        });

        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        state
            .sessions
            .insert(session.session_id.id.as_str().to_string(), session);

        Ok(serde_json::to_value(result)?)
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, ServiceState>, ServiceError> {
        self.state
            .lock()
            .map_err(|_| ServiceError::Rpc("service state lock poisoned".to_string()))
    }
}

#[derive(Default)]
struct ServiceState {
    sessions: HashMap<String, ManagedSession>,
    operations: HashMap<String, ServiceOperation>,
}

#[derive(Clone, Debug, Serialize)]
struct ManagedSession {
    session_id: SessionRef,
    capability: String,
    project_root: PathBuf,
    internal_workspace_root: PathBuf,
    artifact_dir: PathBuf,
    target: DebugTarget,
    state: DebugSessionState,
    worker: WorkerHandle,
    created_at: Timestamp,
    updated_at: Timestamp,
    last_operation: Option<OperationRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerIdentity {
    LocalSystem,
    UserSession,
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
    pub target: DebugTarget,
}

pub trait WorkerSupervisor: Send + Sync {
    fn create_debug_worker(
        &self,
        request: WorkerCreateRequest,
    ) -> Result<WorkerHandle, ServiceError>;
    fn close_worker(&self, worker: &WorkerHandle) -> Result<(), ServiceError>;
    fn kill_worker(&self, worker: &WorkerHandle) -> Result<(), ServiceError>;
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
    fn create_debug_worker(
        &self,
        request: WorkerCreateRequest,
    ) -> Result<WorkerHandle, ServiceError> {
        let worker_id = Id::new(format!("worker-{}", request.session_id.id.as_str()))
            .expect("generated worker ids are valid");
        Ok(WorkerHandle {
            worker_id,
            pipe_name: format!(r"\\.\pipe\dbgatlas-{}", request.session_id.id.as_str()),
            session_id: request.session_id,
            identity: self.identity.clone(),
        })
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
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
    pub events: Vec<ServiceEvent>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl ServiceOperation {
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
struct DebugEvalParams {
    session_id: SessionRef,
    command: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct OperationGetParams {
    operation_id: OperationRef,
}

#[derive(Clone, Copy)]
enum SessionFinishMode {
    Close,
    Kill,
}

pub fn run_http_service(config: ServiceConfig, host: ServiceHost) -> Result<(), ServiceError> {
    validate_config(&config)?;
    let listener = TcpListener::bind(config.bind)?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        let response = match handle_http_stream(&mut stream, &config, &host) {
            Ok(response) => response,
            Err(error) => http_json_response(
                http_status_for(&error),
                &JsonRpcResponse::error(None, rpc_error_for(error)),
            )?,
        };
        stream.write_all(response.as_bytes())?;
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
    let response = host.handle_rpc(rpc);
    http_json_response(200, &response)
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
    if request.path != "/rpc" {
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

fn session_relative_path(session_id: &SessionRef, suffix: &str) -> PathBuf {
    PathBuf::from("artifacts")
        .join("sessions")
        .join(session_id.id.as_str())
        .join(suffix)
}

fn next_session_ref() -> SessionRef {
    let count = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    SessionRef::new(
        Id::new(format!("session-{}-{count}", Timestamp::now().unix_millis))
            .expect("generated session ids are valid"),
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
        ServiceError::UnsupportedHttpMethod(_) => -32600,
        ServiceError::Rpc(_) | ServiceError::Json(_) => -32602,
        _ => -32000,
    };
    JsonRpcError {
        code,
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
    use windows_sys::Win32::System::JobObjects::CreateJobObjectW;

    #[cfg(windows)]
    pub struct ManagedJob {
        handle: HANDLE,
    }

    unsafe impl Send for ManagedJob {}
    unsafe impl Sync for ManagedJob {}

    #[cfg(windows)]
    impl ManagedJob {
        pub fn create(_name: &str) -> Self {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            Self { handle }
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(eval_operation.artifacts.len(), 2);
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
    fn read_memory_is_not_exposed_until_real_memory_artifacts_exist() {
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

        assert!(response.error.unwrap().message.contains("unknown method"));
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
}
