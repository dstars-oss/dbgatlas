use dbgatlas_debug::{
    AddSymbolsRequest, CreateDebugSession, DebugCommandResult, DebugMemoryResult,
    DebugSessionState, DebugTarget, EvalDebugCommand, ReadMemoryRequest,
};
use dbgatlas_model::{ArtifactRef, Id, OperationRef, SessionRef, Timestamp};
use dbgatlas_worker_protocol::{
    WorkerArtifactWrite, WorkerEnvelope, WorkerProtocolError, WorkerRequest, WorkerResponse,
    decode_jsonl, encode_jsonl,
};
use dbgatlas_workspace::{
    ArtifactMetadata, OperationRecord, OperationStatus, Workspace, WorkspaceError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub const INTERNAL_WORKSPACE_DIR: &str = "dbgatlas";
pub const DEFAULT_SERVICE_PORT: u16 = 7331;
pub const MAX_MEMORY_READ_LENGTH: u64 = 16 * 1024 * 1024;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
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
    #[error("worker error: {0}")]
    Worker(String),
    #[error("worker transport is not supported on this platform")]
    WorkerTransportUnsupported,
    #[error(transparent)]
    Debug(#[from] dbgatlas_debug::DebugError),
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

    pub fn with_process_workers() -> Result<Self, ServiceError> {
        Ok(Self::new(Arc::new(ProcessWorkerSupervisor::new()?)))
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
            "debug.modules" => self.debug_builtin_eval(request.params, "debug.modules", "lm"),
            "debug.threads" => self.debug_builtin_eval(request.params, "debug.threads", "~"),
            "debug.stack" => self.debug_builtin_eval(request.params, "debug.stack", "k"),
            "debug.add_symbols" => self.debug_add_symbols(request.params),
            "debug.read_memory" => self.debug_read_memory(request.params),
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
            target,
            state: DebugSessionState::Ready,
            worker,
            request_lock: Arc::new(Mutex::new(())),
            created_at: now,
            updated_at: now,
            last_operation: Some(operation_id.clone()),
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
            worker_response,
        )
    }

    fn add_symbols(&self, request: AddSymbolsRequest) -> Result<Value, ServiceError> {
        let session = self.reusable_session(&request.session_id)?;
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = self.reusable_session(&request.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
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
            worker_response,
        )
    }

    fn read_memory(&self, request: ReadMemoryRequest) -> Result<Value, ServiceError> {
        let session = self.reusable_session(&request.session_id)?;
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = self.reusable_session(&request.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
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

        self.finish_memory_worker_response(&session, &workspace, operation_id, worker_response)
    }

    fn finish_command_worker_response(
        &self,
        session: &ManagedSession,
        workspace: &Workspace,
        operation_id: OperationRef,
        capability: &'static str,
        worker_response: Result<WorkerResponse, ServiceError>,
    ) -> Result<Value, ServiceError> {
        match worker_response {
            Ok(WorkerResponse::DebugCommand { mut result, writes }) => {
                let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
                result.operation_id = Some(operation_id.clone());
                result.raw_output = registered.raw_output.clone();
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
                    status: workspace_status,
                    created_at: Timestamp::now(),
                    summary: summary.clone(),
                    artifacts: registered.artifacts.clone(),
                })?;

                let mut state = self.lock_state()?;
                if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
                    if !was_canceled {
                        operation.status = ServiceOperationStatus::Success;
                    }
                    operation.summary = summary;
                    operation.artifacts = registered.artifacts;
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

                Ok(serde_json::to_value(result)?)
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
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    registered.artifacts,
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
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    Vec::new(),
                )?;
                Err(ServiceError::Worker(message))
            }
            Err(error) => {
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: if was_canceled {
                        OperationStatus::Canceled
                    } else {
                        OperationStatus::Failed
                    },
                    created_at: Timestamp::now(),
                    summary: error.to_string(),
                    artifacts: Vec::new(),
                })?;
                if !was_canceled {
                    self.finish_operation_in_memory(
                        &operation_id,
                        ServiceOperationStatus::Failed,
                        error.to_string(),
                        Vec::new(),
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
                    status,
                    created_at: Timestamp::now(),
                    summary: summary.to_string(),
                    artifacts: registered.artifacts.clone(),
                })?;

                let mut state = self.lock_state()?;
                if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
                    if !was_canceled {
                        operation.status = ServiceOperationStatus::Success;
                    }
                    operation.summary = summary.to_string();
                    operation.artifacts = registered.artifacts;
                    operation.updated_at = Timestamp::now();
                }
                if let Some(session) = state.sessions.get_mut(session.session_id.id.as_str()) {
                    session.last_operation = Some(operation_id.clone());
                    session.updated_at = Timestamp::now();
                }

                Ok(serde_json::to_value(result)?)
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
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    registered.artifacts,
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
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    Vec::new(),
                )?;
                Err(ServiceError::Worker(message))
            }
            Err(error) => {
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: if was_canceled {
                        OperationStatus::Canceled
                    } else {
                        OperationStatus::Failed
                    },
                    created_at: Timestamp::now(),
                    summary: error.to_string(),
                    artifacts: Vec::new(),
                })?;
                if !was_canceled {
                    self.finish_operation_in_memory(
                        &operation_id,
                        ServiceOperationStatus::Failed,
                        error.to_string(),
                        Vec::new(),
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
    ) -> Result<(), ServiceError> {
        let mut state = self.lock_state()?;
        if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
            operation.status = status;
            operation.summary = summary;
            operation.artifacts = artifacts;
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

    fn reusable_session(&self, session_id: &SessionRef) -> Result<ManagedSession, ServiceError> {
        let state = self.lock_state()?;
        let session = state
            .sessions
            .get(session_id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::SessionNotFound(session_id.to_string()))?;
        if !session.state.is_reusable() {
            return Err(ServiceError::SessionNotReusable(session_id.to_string()));
        }
        Ok(session)
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
    #[serde(skip)]
    request_lock: Arc<Mutex<()>>,
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
    pub startup_timeout_ms: u64,
}

pub trait WorkerSupervisor: Send + Sync {
    fn create_debug_worker(
        &self,
        request: WorkerCreateRequest,
    ) -> Result<WorkerHandle, ServiceError>;
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
    workers: Mutex<HashMap<String, Arc<ProcessWorkerState>>>,
    job: job::ManagedJob,
}

struct ProcessWorkerState {
    child: Mutex<Child>,
    transport: Mutex<WorkerTransport>,
}

impl ProcessWorkerSupervisor {
    pub fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            identity: WorkerIdentity::CurrentUserDevMode,
            workers: Mutex::new(HashMap::new()),
            job: job::ManagedJob::create_result("DbgAtlasDevWorkers")?,
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
    fn create_debug_worker(
        &self,
        request: WorkerCreateRequest,
    ) -> Result<WorkerHandle, ServiceError> {
        let worker_id = Id::new(format!("worker-{}", request.session_id.id.as_str()))
            .expect("generated worker ids are valid");
        let pipe_name = unique_pipe_name(&request.session_id);
        let transport = WorkerTransport::create_server(&pipe_name)?;
        let worker_exe = worker_executable_path()?;
        let mut child = Command::new(worker_exe)
            .arg("--pipe")
            .arg(&pipe_name)
            .arg("--session-id")
            .arg(request.session_id.id.as_str())
            .spawn()?;
        self.job.assign_child(&child)?;
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
    let handle = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(WorkerPipeServer { handle })
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

        pub fn assign_child(&self, child: &std::process::Child) -> Result<(), std::io::Error> {
            use std::os::windows::io::AsRawHandle;

            if self.handle.is_null() {
                return Ok(());
            }
            let ok =
                unsafe { AssignProcessToJobObject(self.handle, child.as_raw_handle() as HANDLE) };
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

        pub fn assign_child(&self, _child: &std::process::Child) -> Result<(), std::io::Error> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

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
        assert_eq!(eval_operation.artifacts.len(), 3);
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
        assert_eq!(response.result.as_ref().unwrap()["bytes_read"], 16);

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

    impl WorkerSupervisor for FailingStartSupervisor {
        fn create_debug_worker(
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
        fn create_debug_worker(
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
