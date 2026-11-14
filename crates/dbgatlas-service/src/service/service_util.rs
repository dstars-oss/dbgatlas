use super::*;

pub(super) fn validate_config(config: &ServiceConfig) -> Result<(), ServiceError> {
    if !config.bind.ip().is_loopback() {
        return Err(ServiceError::NonLoopbackBind(config.bind));
    }
    if config.bearer_token.trim().is_empty() {
        return Err(ServiceError::EmptyBearerToken);
    }
    Ok(())
}

pub(super) fn parse_params<T: for<'de> Deserialize<'de>>(
    params: Option<Value>,
) -> Result<T, ServiceError> {
    serde_json::from_value(params.unwrap_or(Value::Object(Default::default()))).map_err(Into::into)
}

pub(super) fn parse_debug_session_create_params(
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

pub(super) fn parse_u64_param(value: &Value, field: &'static str) -> Result<u64, ServiceError> {
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

pub(super) struct DebugStartFailure {
    pub(super) code: String,
    pub(super) message: String,
}

pub(super) struct DebugSessionStartAttempt {
    pub(super) worker: Option<WorkerHandle>,
    pub(super) writes: Option<Vec<WorkerArtifactWrite>>,
    pub(super) failures: Vec<String>,
}

impl std::fmt::Display for DebugStartFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

pub(super) fn debug_start_failure_message(
    code: String,
    message: String,
    worker_identity: DebugWorkerIdentity,
) -> DebugStartFailure {
    DebugStartFailure {
        code,
        message: add_debug_access_denied_hint(message, worker_identity),
    }
}

pub(super) fn should_auto_retry_active_interactive_user(
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

pub(super) fn debug_target_can_need_active_interactive_user(target: &DebugTarget) -> bool {
    matches!(target, DebugTarget::File { .. }) || matches!(target, DebugTarget::Launch { .. })
}

pub(super) fn debug_worker_identity_label(identity: Option<&WorkerIdentity>) -> &'static str {
    match identity {
        None => "default",
        Some(WorkerIdentity::LocalSystem) => "local_system",
        Some(WorkerIdentity::ActiveInteractiveUser) => "active_interactive_user",
        Some(WorkerIdentity::CurrentUserDevMode) => "current_user_dev_mode",
    }
}

pub(super) fn add_debug_access_denied_hint(
    message: String,
    worker_identity: DebugWorkerIdentity,
) -> String {
    if worker_identity == DebugWorkerIdentity::Default && is_access_denied_message(&message) {
        format!(
            "{message}; recommended_retry_worker_identity=active_interactive_user; if this is an installed service using the default LocalSystem debug worker, retry debug.session.create with worker_identity:\"active_interactive_user\" for user-session dump files, TTD replay, or live launch"
        )
    } else {
        message
    }
}

pub(super) fn is_access_denied_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("access is denied")
        || lower.contains("os error 5")
        || lower.contains("0x80070005")
        || lower.contains("e_accessdenied")
        || message.contains("拒绝访问")
}

pub(super) fn validate_optional_timeout_ms(timeout_ms: Option<u64>) -> Result<(), ServiceError> {
    if timeout_ms == Some(0) {
        return Err(ServiceError::Rpc(
            "timeout_ms must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn session_relative_path(session_id: &SessionRef, suffix: &str) -> PathBuf {
    PathBuf::from("artifacts")
        .join("sessions")
        .join(session_id.id.as_str())
        .join(suffix)
}

pub(super) fn recording_relative_path(recording_id: &RecordingRef, suffix: &str) -> PathBuf {
    PathBuf::from("artifacts")
        .join("recordings")
        .join(recording_id.id.as_str())
        .join(suffix)
}

pub(super) fn etw_session_name(recording_id: &RecordingRef) -> String {
    format!("DbgAtlas-{}", recording_id.id.as_str())
}

pub(super) fn next_session_ref() -> SessionRef {
    let count = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    SessionRef::new(
        Id::new(format!("session-{}-{count}", Timestamp::now().unix_millis))
            .expect("generated session ids are valid"),
    )
}

pub(super) fn next_recording_ref() -> RecordingRef {
    let count = RECORDING_COUNTER.fetch_add(1, Ordering::Relaxed);
    RecordingRef::new(
        Id::new(format!(
            "recording-{}-{count}",
            Timestamp::now().unix_millis
        ))
        .expect("generated recording ids are valid"),
    )
}

pub(super) fn next_operation_ref() -> OperationRef {
    let count = OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    OperationRef::new(
        Id::new(format!("op-{}-{count}", Timestamp::now().unix_millis))
            .expect("generated operation ids are valid"),
    )
}

pub(super) fn next_artifact_ref() -> ArtifactRef {
    let count = ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed);
    ArtifactRef::new(
        Id::new(format!("artifact-{}-{count}", Timestamp::now().unix_millis))
            .expect("generated artifact ids are valid"),
    )
}

pub(super) fn rpc_error_for(error: ServiceError) -> JsonRpcError {
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

pub(super) fn mcp_error_for(error: ServiceError) -> JsonRpcError {
    JsonRpcError {
        code: -32000,
        message: error.to_string(),
    }
}

pub(super) fn http_status_for(error: &ServiceError) -> u16 {
    match error {
        ServiceError::Unauthorized => 401,
        ServiceError::InvalidOrigin(_) => 403,
        ServiceError::UnsupportedHttpMethod(_) => 405,
        _ => 400,
    }
}

pub(super) fn is_allowed_origin(origin: &str) -> bool {
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

pub(super) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|candidate| candidate == needle)
}
