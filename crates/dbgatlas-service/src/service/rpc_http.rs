use super::*;

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
    pub(super) fn running(
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

    pub(super) fn success(
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

    pub(super) fn failed(
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
    pub(super) fn result(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(super) fn error(id: Option<Value>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

pub(super) struct ToolCallOutput {
    pub(super) value: Value,
    pub(super) is_error: bool,
    pub(super) operation_id: Option<String>,
    pub(super) error_code: Option<i64>,
    pub(super) error_message: Option<String>,
}

impl ToolCallOutput {
    pub(super) fn success(value: Value) -> Self {
        let operation_id = extract_ref_id_field(&value, "operation_id");
        Self {
            value,
            is_error: false,
            operation_id,
            error_code: None,
            error_message: None,
        }
    }

    pub(super) fn error(value: Value) -> Self {
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

    pub(super) fn set_operation_id(&mut self, operation_id: String) {
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

pub(super) fn mcp_service_response_result(response: JsonRpcResponse) -> ToolCallOutput {
    if let Some(error) = response.error {
        return ToolCallOutput::error(json!({
            "error": {
                "code": error.code,
                "message": error.message,
            }
        }));
    }
    ToolCallOutput::success(response.result.unwrap_or(Value::Null))
}

#[derive(Default)]
pub(super) struct DiagnosticContext {
    pub(super) remote_addr: Option<String>,
    pub(super) path: Option<String>,
    pub(super) rpc_method: Option<String>,
    pub(super) rpc_id: Option<String>,
    pub(super) mcp_tool: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) operation_id: Option<String>,
}

impl DiagnosticContext {
    pub(super) fn from_rpc(request: &JsonRpcRequest) -> Self {
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

    pub(super) fn log_fields(&self) -> String {
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

pub(super) fn append_mcp_tool_error_log(name: &str, arguments: &Value, result: &ToolCallOutput) {
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

pub(super) fn append_http_diagnostic_log(stage: &str, context: &DiagnosticContext, error: &str) {
    append_service_diagnostic_log(&format!(
        "http_request_error stage={} {} error={}",
        sanitize_log_value(stage),
        context.log_fields(),
        sanitize_log_value(error)
    ));
}

pub(super) fn append_http_success_log(
    status: u16,
    duration: Duration,
    context: &DiagnosticContext,
) {
    append_service_diagnostic_log(&format!(
        "http_request_complete status={} duration_ms={} {}",
        status,
        duration.as_millis(),
        context.log_fields()
    ));
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_worker_transport_log(
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

pub(super) fn worker_response_kind(response: &WorkerResponse) -> &'static str {
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

pub(super) fn append_operation_diagnostic_log(
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

pub(super) fn append_service_diagnostic_log(message: &str) {
    append_service_diagnostic_log_for_paths(&default_windows_service_paths(), message);
}

pub(super) fn append_service_diagnostic_log_for_paths(paths: &ServiceInstallPaths, message: &str) {
    let timestamp = Timestamp::now().unix_millis;
    let day = (timestamp / 86_400_000) as i64;
    let log_path = paths
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

pub(super) fn log_value(value: Option<&str>) -> String {
    value
        .map(sanitize_log_value)
        .unwrap_or_else(|| "unknown".to_string())
}

pub(super) fn rpc_id_for_log(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => "null".to_string(),
        _ => "complex".to_string(),
    }
}

pub(super) fn sanitize_log_value(value: &str) -> String {
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

pub(super) fn extract_ref_id_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(extract_ref_id_value)
}

pub(super) fn extract_ref_id_value(value: &Value) -> Option<String> {
    value.as_str().map(ToOwned::to_owned).or_else(|| {
        value
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

pub(super) fn log_utc_date_from_unix_day(day: i64) -> String {
    let (year, month, day) = log_date_from_unix_day(day);
    format!("{year:04}-{month:02}-{day:02}")
}

pub(super) fn log_date_from_unix_day(day: i64) -> (i32, u32, u32) {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct DebugSessionCreateParams {
    pub(super) project_root: PathBuf,
    pub(super) target: DebugTarget,
    #[serde(default)]
    pub(super) startup_timeout_ms: Option<u64>,
    #[serde(default)]
    pub(super) worker_identity: DebugWorkerIdentity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DebugWorkerIdentity {
    #[default]
    Default,
    ActiveInteractiveUser,
}

impl DebugWorkerIdentity {
    pub(super) fn worker_create_identity(self) -> Option<WorkerIdentity> {
        match self {
            Self::Default => None,
            Self::ActiveInteractiveUser => Some(WorkerIdentity::ActiveInteractiveUser),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct SessionParams {
    pub(super) session_id: SessionRef,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct RecordingStartParams {
    pub(super) project_root: PathBuf,
    pub(super) target: RecordingTarget,
    #[serde(default = "dbgatlas_recording::default_presets")]
    pub(super) presets: Vec<RecordingPreset>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct RecordingTtdParams {
    pub(super) project_root: PathBuf,
    pub(super) target: TtdTarget,
    pub(super) timeout_ms: u64,
    #[serde(default)]
    pub(super) options: TtdRecordingOptions,
    #[serde(default)]
    pub(super) worker_identity: TtdRecorderIdentity,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct RecordingParams {
    pub(super) recording_id: RecordingRef,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct DebugEvalParams {
    pub(super) session_id: SessionRef,
    pub(super) command: String,
    #[serde(default)]
    pub(super) timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct DebugEvalStepsParams {
    pub(super) session_id: SessionRef,
    pub(super) commands: Vec<String>,
    #[serde(default)]
    pub(super) timeout_ms: Option<u64>,
    #[serde(default)]
    pub(super) continue_on_error: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct DebugAddSymbolsParams {
    pub(super) session_id: SessionRef,
    pub(super) symbol_path: String,
    #[serde(default)]
    pub(super) reload: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct DebugReadMemoryParams {
    pub(super) session_id: SessionRef,
    pub(super) address: Value,
    pub(super) length: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ReverseSessionOpenParams {
    pub(super) project_root: PathBuf,
    pub(super) database_path: PathBuf,
    #[serde(default)]
    pub(super) ida_install_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ReverseLookupFunctionParams {
    pub(super) session_id: SessionRef,
    pub(super) runtime_address: Value,
    pub(super) runtime_module_base: Value,
    pub(super) ida_image_base: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ReverseCoreFunctionParams {
    pub(super) session_id: SessionRef,
    #[serde(flatten)]
    pub(super) arguments: HashMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ReverseSessionCloseParams {
    pub(super) session_id: SessionRef,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ServiceUpdateParams {
    pub(super) source_dir: PathBuf,
    #[serde(default = "default_service_update_restart")]
    pub(super) restart: bool,
    #[serde(default = "default_service_update_timeout_ms")]
    pub(super) timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct OperationGetParams {
    pub(super) operation_id: OperationRef,
}

#[derive(Debug, Deserialize)]
pub(super) struct ToolCallParams {
    pub(super) name: String,
    #[serde(default)]
    pub(super) arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectRootParams {
    pub(super) project_root: PathBuf,
}

pub(super) fn default_service_update_restart() -> bool {
    true
}

pub(super) fn default_service_update_timeout_ms() -> u64 {
    DEFAULT_SERVICE_UPDATE_TIMEOUT_MS
}

#[derive(Clone, Copy)]
pub(super) enum SessionFinishMode {
    Close,
    Kill,
}

#[derive(Clone, Copy)]
pub(super) enum RecordingFinishMode {
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
        shutdown.handler_started();
        let handler_shutdown = shutdown.clone();
        std::thread::spawn(move || {
            let _handler_guard = HttpHandlerGuard(handler_shutdown);
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
    if !shutdown.wait_for_handlers(WORKER_SHUTDOWN_TIMEOUT) {
        append_service_diagnostic_log(&format!(
            "http_service_stop_active_handlers_timeout timeout_ms={}",
            WORKER_SHUTDOWN_TIMEOUT.as_millis()
        ));
    }
    host.shutdown_workers_gracefully(WORKER_SHUTDOWN_TIMEOUT);
    Ok(())
}

pub(super) struct HttpHandlerGuard(ServiceShutdown);

impl Drop for HttpHandlerGuard {
    fn drop(&mut self) {
        self.0.handler_finished();
    }
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

pub(super) fn handle_http_stream(
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

pub(super) fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, ServiceError> {
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

pub(super) fn authorize_http_request(
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
    match request.headers.get("authorization") {
        Some(actual) if bearer_token_matches(actual, &config.bearer_token) => Ok(()),
        _ => Err(ServiceError::Unauthorized),
    }
}

pub(super) fn bearer_token_matches(actual: &str, expected_token: &str) -> bool {
    let mut parts = actual.split_ascii_whitespace();
    let Some(scheme) = parts.next() else {
        return false;
    };
    let Some(token) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    scheme.eq_ignore_ascii_case("Bearer") && token == expected_token
}

pub(super) fn http_json_response(
    status: u16,
    value: &JsonRpcResponse,
) -> Result<String, ServiceError> {
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

pub(super) fn http_empty_response(status: u16, reason: &str) -> String {
    format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: HashMap<String, String>,
    pub(super) body: Vec<u8>,
}
