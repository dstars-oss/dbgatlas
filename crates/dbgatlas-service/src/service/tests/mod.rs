use super::*;
use serde_json::json;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

mod host;
mod http;
mod install;
mod recording;
mod reverse;

struct TestServer {
    endpoint: SocketAddr,
    shutdown: ServiceShutdown,
    handle: std::thread::JoinHandle<Result<(), ServiceError>>,
    _guard: std::sync::MutexGuard<'static, ()>,
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
    static HTTP_TEST_LOCK: Mutex<()> = Mutex::new(());
    let guard = HTTP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        _guard: guard,
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
    let authorization = token.map(|token| format!("Bearer {token}"));
    post_json_with_authorization_header(endpoint, path, authorization.as_deref(), origin, body)
}

fn post_json_with_authorization_header(
    endpoint: SocketAddr,
    path: &str,
    authorization: Option<&str>,
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
    if let Some(authorization) = authorization {
        write!(stream, "Authorization: {authorization}\r\n").unwrap();
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
#[allow(clippy::enum_variant_names)]
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
        .find_map(|pair| (pair[0] == "-out").then(|| PathBuf::from(&pair[1])))
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

struct ShutdownRecordingSupervisor {
    requests: Mutex<Vec<&'static str>>,
    close_count: AtomicU64,
    kill_count: AtomicU64,
}

impl ShutdownRecordingSupervisor {
    fn new() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            close_count: AtomicU64::new(0),
            kill_count: AtomicU64::new(0),
        }
    }
}

impl WorkerSupervisor for RecordingIdentitySupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        self.identities
            .lock()
            .unwrap()
            .push(request.identity.clone());
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        self.identities
            .lock()
            .unwrap()
            .push(request.identity.clone());
        let identity = request.identity.unwrap_or(WorkerIdentity::LocalSystem);
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
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
    fn create_worker(&self, _request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
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

impl WorkerSupervisor for ShutdownRecordingSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(test_worker_handle(request.session_id))
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        self.requests.lock().unwrap().push(request.method_name());
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
        self.close_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        self.kill_count.fetch_add(1, Ordering::SeqCst);
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
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
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
