use dbgatlas_debug::{DebugCommandResult, DebugMemoryResult, DebugTarget};
use dbgatlas_model::{OperationRef, ResultPreview, SessionRef, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use thiserror::Error;

pub const WORKER_PROTOCOL_VERSION: u32 = 4;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerEnvelope<T> {
    pub version: u32,
    pub request_id: String,
    pub message: T,
}

impl<T> WorkerEnvelope<T> {
    pub fn new(request_id: impl Into<String>, message: T) -> Self {
        Self {
            version: WORKER_PROTOCOL_VERSION,
            request_id: request_id.into(),
            message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum WorkerRequest {
    StartDebugSession {
        session_id: SessionRef,
        target: DebugTarget,
        artifact_dir: PathBuf,
    },
    EvalDebugCommand {
        session_id: SessionRef,
        operation_id: OperationRef,
        command: String,
        artifact_dir: PathBuf,
    },
    AddSymbols {
        session_id: SessionRef,
        operation_id: OperationRef,
        symbol_path: String,
        reload: bool,
        artifact_dir: PathBuf,
    },
    ReadMemory {
        session_id: SessionRef,
        operation_id: OperationRef,
        address: u64,
        length: u64,
        artifact_dir: PathBuf,
    },
    OpenReverseSession {
        session_id: SessionRef,
        ida_install_dir: PathBuf,
        database_path: PathBuf,
    },
    LookupReverseFunction {
        session_id: SessionRef,
        runtime_address: u64,
        runtime_module_base: u64,
        ida_image_base: u64,
    },
    ReverseCoreFunction {
        session_id: SessionRef,
        operation_id: OperationRef,
        function: String,
        arguments: Value,
        artifact_dir: PathBuf,
    },
    CloseReverseSession {
        session_id: SessionRef,
    },
    CloseSession {
        session_id: SessionRef,
    },
    KillSession {
        session_id: SessionRef,
    },
    CancelOperation {
        session_id: SessionRef,
        operation_id: OperationRef,
    },
}

impl WorkerRequest {
    pub fn method_name(&self) -> &'static str {
        match self {
            WorkerRequest::StartDebugSession { .. } => "start_debug_session",
            WorkerRequest::EvalDebugCommand { .. } => "eval_debug_command",
            WorkerRequest::AddSymbols { .. } => "add_symbols",
            WorkerRequest::ReadMemory { .. } => "read_memory",
            WorkerRequest::OpenReverseSession { .. } => "open_reverse_session",
            WorkerRequest::LookupReverseFunction { .. } => "lookup_reverse_function",
            WorkerRequest::ReverseCoreFunction { .. } => "reverse_core_function",
            WorkerRequest::CloseReverseSession { .. } => "close_reverse_session",
            WorkerRequest::CloseSession { .. } => "close_session",
            WorkerRequest::KillSession { .. } => "kill_session",
            WorkerRequest::CancelOperation { .. } => "cancel_operation",
        }
    }

    pub fn session_id(&self) -> &SessionRef {
        match self {
            WorkerRequest::StartDebugSession { session_id, .. }
            | WorkerRequest::EvalDebugCommand { session_id, .. }
            | WorkerRequest::AddSymbols { session_id, .. }
            | WorkerRequest::ReadMemory { session_id, .. }
            | WorkerRequest::OpenReverseSession { session_id, .. }
            | WorkerRequest::LookupReverseFunction { session_id, .. }
            | WorkerRequest::ReverseCoreFunction { session_id, .. }
            | WorkerRequest::CloseReverseSession { session_id }
            | WorkerRequest::CloseSession { session_id }
            | WorkerRequest::KillSession { session_id }
            | WorkerRequest::CancelOperation { session_id, .. } => session_id,
        }
    }

    pub fn operation_id(&self) -> Option<&OperationRef> {
        match self {
            WorkerRequest::EvalDebugCommand { operation_id, .. }
            | WorkerRequest::AddSymbols { operation_id, .. }
            | WorkerRequest::ReadMemory { operation_id, .. }
            | WorkerRequest::ReverseCoreFunction { operation_id, .. }
            | WorkerRequest::CancelOperation { operation_id, .. } => Some(operation_id),
            WorkerRequest::StartDebugSession { .. }
            | WorkerRequest::OpenReverseSession { .. }
            | WorkerRequest::LookupReverseFunction { .. }
            | WorkerRequest::CloseReverseSession { .. }
            | WorkerRequest::CloseSession { .. }
            | WorkerRequest::KillSession { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerResponse {
    Ok {
        summary: String,
        writes: Vec<WorkerArtifactWrite>,
    },
    DebugCommand {
        result: DebugCommandResult,
        writes: Vec<WorkerArtifactWrite>,
    },
    DebugMemory {
        result: DebugMemoryResult,
        writes: Vec<WorkerArtifactWrite>,
    },
    ReverseSessionOpened {
        writes: Vec<WorkerArtifactWrite>,
    },
    ReverseFunctionLookup {
        result: ReverseFunctionLookupResult,
        writes: Vec<WorkerArtifactWrite>,
    },
    ReverseCoreFunction {
        result: ReverseCoreFunctionResult,
        writes: Vec<WorkerArtifactWrite>,
    },
    Failed {
        code: String,
        message: String,
        writes: Vec<WorkerArtifactWrite>,
    },
}

impl WorkerResponse {
    pub fn ok(summary: impl Into<String>) -> Self {
        Self::Ok {
            summary: summary.into(),
            writes: Vec::new(),
        }
    }

    pub fn ok_with_writes(summary: impl Into<String>, writes: Vec<WorkerArtifactWrite>) -> Self {
        Self::Ok {
            summary: summary.into(),
            writes,
        }
    }

    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Failed {
            code: code.into(),
            message: message.into(),
            writes: Vec::new(),
        }
    }

    pub fn writes(&self) -> &[WorkerArtifactWrite] {
        match self {
            WorkerResponse::Ok { writes, .. }
            | WorkerResponse::DebugCommand { writes, .. }
            | WorkerResponse::DebugMemory { writes, .. }
            | WorkerResponse::ReverseSessionOpened { writes }
            | WorkerResponse::ReverseFunctionLookup { writes, .. }
            | WorkerResponse::ReverseCoreFunction { writes, .. }
            | WorkerResponse::Failed { writes, .. } => writes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReverseCoreFunctionResult {
    pub function: String,
    pub result: ResultPreview<Value>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseFunctionLookupResult {
    pub runtime_address: u64,
    pub runtime_module_base: u64,
    pub rva: u64,
    pub ida_image_base: u64,
    pub ida_ea: u64,
    pub function_start: u64,
    pub function_end: u64,
    pub function_name: String,
    pub found: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerArtifactWrite {
    pub relative_path: PathBuf,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<WorkerArtifactRole>,
    pub byte_len: u64,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerArtifactRole {
    FullResult,
}

pub fn json_result_preview(
    value: &Value,
    full_byte_len: u64,
    byte_limit: usize,
) -> Result<ResultPreview<Value>, serde_json::Error> {
    if serde_json::to_vec(value)?.len() <= byte_limit {
        return Ok(ResultPreview {
            value: value.clone(),
            truncated: false,
            full_byte_len,
            inline_byte_limit: byte_limit as u64,
        });
    }

    let preview = match value {
        Value::Object(object) if object.get("pseudocode").is_some_and(Value::is_string) => {
            let mut preview_object = object.clone();
            let pseudocode = object["pseudocode"].as_str().unwrap_or_default();
            let mut end = pseudocode.len().min(byte_limit);
            let mut fits = false;
            while end > 0 {
                while end > 0 && !pseudocode.is_char_boundary(end) {
                    end -= 1;
                }
                preview_object.insert(
                    "pseudocode".to_string(),
                    Value::String(pseudocode[..end].to_string()),
                );
                if serde_json::to_vec(&preview_object)?.len() <= byte_limit {
                    fits = true;
                    break;
                }
                end = end.saturating_sub((end / 8).max(1));
            }
            if !fits {
                preview_object.insert("pseudocode".to_string(), Value::String(String::new()));
                fits = serde_json::to_vec(&preview_object)?.len() <= byte_limit;
            }
            if fits {
                Value::Object(preview_object)
            } else {
                Value::Null
            }
        }
        Value::Object(object) if object.get("instructions").is_some_and(Value::is_array) => {
            let mut preview_object = object.clone();
            let instructions = object["instructions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            preview_object.insert("instructions".to_string(), Value::Array(Vec::new()));
            let mut included = Vec::new();
            for instruction in instructions {
                included.push(instruction);
                preview_object.insert("instructions".to_string(), Value::Array(included.clone()));
                if serde_json::to_vec(&preview_object)?.len() > byte_limit {
                    included.pop();
                    break;
                }
            }
            preview_object.insert("instructions".to_string(), Value::Array(included));
            Value::Object(preview_object)
        }
        _ => Value::Null,
    };

    Ok(ResultPreview {
        value: preview,
        truncated: true,
        full_byte_len,
        inline_byte_limit: byte_limit as u64,
    })
}

pub fn write_atomic_bytes(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut part_name = path.as_os_str().to_os_string();
    part_name.push(".part");
    let part_path = PathBuf::from(part_name);
    let result = (|| {
        let mut file = std::fs::File::create(&part_path)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
        std::fs::rename(&part_path, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part_path);
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkerEvent {
    StateChanged {
        session_id: SessionRef,
        state: String,
        timestamp: Timestamp,
    },
    Output {
        session_id: SessionRef,
        operation_id: OperationRef,
        text: String,
        timestamp: Timestamp,
    },
}

#[derive(Debug, Error)]
pub enum WorkerProtocolError {
    #[error("unsupported worker protocol version {actual}; expected {expected}")]
    UnsupportedVersion { actual: u32, expected: u32 },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn encode_jsonl<T: Serialize>(
    message: &WorkerEnvelope<T>,
) -> Result<String, WorkerProtocolError> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    Ok(line)
}

pub fn decode_jsonl<T: for<'de> Deserialize<'de>>(
    line: &str,
) -> Result<WorkerEnvelope<T>, WorkerProtocolError> {
    let envelope: WorkerEnvelope<T> = serde_json::from_str(line)?;
    if envelope.version != WORKER_PROTOCOL_VERSION {
        return Err(WorkerProtocolError::UnsupportedVersion {
            actual: envelope.version,
            expected: WORKER_PROTOCOL_VERSION,
        });
    }
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgatlas_model::{Id, OperationRef, SessionRef};

    #[test]
    fn request_round_trips_as_jsonl() {
        let request = WorkerEnvelope::new(
            "req-001",
            WorkerRequest::CloseSession {
                session_id: SessionRef::new(Id::new("session-001").unwrap()),
            },
        );

        let encoded = encode_jsonl(&request).unwrap();
        assert!(encoded.ends_with('\n'));
        let decoded: WorkerEnvelope<WorkerRequest> = decode_jsonl(&encoded).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn read_memory_round_trips_as_jsonl() {
        let request = WorkerEnvelope::new(
            "req-003",
            WorkerRequest::ReadMemory {
                session_id: SessionRef::new(Id::new("session-001").unwrap()),
                operation_id: OperationRef::new(Id::new("op-001").unwrap()),
                address: 0x1000,
                length: 32,
                artifact_dir: PathBuf::from(r"C:\case\dbgatlas\artifacts\sessions\session-001"),
            },
        );

        let encoded = encode_jsonl(&request).unwrap();
        let decoded: WorkerEnvelope<WorkerRequest> = decode_jsonl(&encoded).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn reverse_requests_round_trip_as_jsonl() {
        let session_id = SessionRef::new(Id::new("session-001").unwrap());
        let open = WorkerEnvelope::new(
            "req-rev-open",
            WorkerRequest::OpenReverseSession {
                session_id: session_id.clone(),
                ida_install_dir: PathBuf::from(r"C:\Program Files\IDA Professional 9.3"),
                database_path: PathBuf::from(r"C:\case\sample.i64"),
            },
        );
        let lookup = WorkerEnvelope::new(
            "req-rev-lookup",
            WorkerRequest::LookupReverseFunction {
                session_id: session_id.clone(),
                runtime_address: 0x180001234,
                runtime_module_base: 0x180000000,
                ida_image_base: 0x140000000,
            },
        );
        let close = WorkerEnvelope::new(
            "req-rev-close",
            WorkerRequest::CloseReverseSession { session_id },
        );

        for request in [open, lookup, close] {
            let encoded = encode_jsonl(&request).unwrap();
            let decoded: WorkerEnvelope<WorkerRequest> = decode_jsonl(&encoded).unwrap();
            assert_eq!(decoded, request);
        }
    }

    #[test]
    fn reverse_core_request_round_trips_as_jsonl() {
        let request = WorkerEnvelope::new(
            "req-rev-core",
            WorkerRequest::ReverseCoreFunction {
                session_id: SessionRef::new(Id::new("session-001").unwrap()),
                operation_id: OperationRef::new(Id::new("op-001").unwrap()),
                function: "rename".to_string(),
                arguments: serde_json::json!({
                    "items": [{
                        "kind": "function",
                        "addr": "0x140001000",
                        "new_name": "dbgatlas_main"
                    }]
                }),
                artifact_dir: PathBuf::from("artifacts"),
            },
        );

        let encoded = encode_jsonl(&request).unwrap();
        let decoded: WorkerEnvelope<WorkerRequest> = decode_jsonl(&encoded).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn reverse_lookup_response_round_trips_as_jsonl() {
        let response = WorkerEnvelope::new(
            "req-rev-lookup",
            WorkerResponse::ReverseFunctionLookup {
                result: ReverseFunctionLookupResult {
                    runtime_address: 0x180001234,
                    runtime_module_base: 0x180000000,
                    rva: 0x1234,
                    ida_image_base: 0x140000000,
                    ida_ea: 0x140001234,
                    function_start: 0x140001200,
                    function_end: 0x140001300,
                    function_name: "sub_140001200".to_string(),
                    found: true,
                },
                writes: Vec::new(),
            },
        );

        let encoded = encode_jsonl(&response).unwrap();
        let decoded: WorkerEnvelope<WorkerResponse> = decode_jsonl(&encoded).unwrap();
        assert_eq!(decoded, response);
    }

    #[test]
    fn reverse_core_response_round_trips_as_jsonl() {
        let response = WorkerEnvelope::new(
            "req-rev-core",
            WorkerResponse::ReverseCoreFunction {
                result: ReverseCoreFunctionResult {
                    function: "list_imports".to_string(),
                    result: ResultPreview {
                        value: serde_json::json!({
                            "offset": 0,
                            "count": 1,
                            "items": [{
                                "module": "KERNEL32.dll",
                                "name": "CreateFileW",
                                "ordinal": null
                            }]
                        }),
                        truncated: false,
                        full_byte_len: 100,
                        inline_byte_limit: 64 * 1024,
                    },
                    warnings: Vec::new(),
                },
                writes: Vec::new(),
            },
        );

        let encoded = encode_jsonl(&response).unwrap();
        let decoded: WorkerEnvelope<WorkerResponse> = decode_jsonl(&encoded).unwrap();
        assert_eq!(decoded, response);
    }

    #[test]
    fn cancel_operation_round_trips_as_jsonl() {
        let request = WorkerEnvelope::new(
            "req-002",
            WorkerRequest::CancelOperation {
                session_id: SessionRef::new(Id::new("session-001").unwrap()),
                operation_id: OperationRef::new(Id::new("op-001").unwrap()),
            },
        );

        let encoded = encode_jsonl(&request).unwrap();
        let decoded: WorkerEnvelope<WorkerRequest> = decode_jsonl(&encoded).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn request_session_id_helper_reads_every_variant() {
        let session_id = SessionRef::new(Id::new("session-001").unwrap());
        let operation_id = OperationRef::new(Id::new("op-001").unwrap());
        let requests = vec![
            WorkerRequest::StartDebugSession {
                session_id: session_id.clone(),
                target: DebugTarget::File {
                    path: PathBuf::from("sample.dmp"),
                },
                artifact_dir: PathBuf::from("artifacts"),
            },
            WorkerRequest::EvalDebugCommand {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                command: ".echo hi".to_string(),
                artifact_dir: PathBuf::from("artifacts"),
            },
            WorkerRequest::AddSymbols {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                symbol_path: "srv*".to_string(),
                reload: false,
                artifact_dir: PathBuf::from("artifacts"),
            },
            WorkerRequest::ReadMemory {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                address: 0x1000,
                length: 16,
                artifact_dir: PathBuf::from("artifacts"),
            },
            WorkerRequest::OpenReverseSession {
                session_id: session_id.clone(),
                ida_install_dir: PathBuf::from("ida"),
                database_path: PathBuf::from("sample.i64"),
            },
            WorkerRequest::LookupReverseFunction {
                session_id: session_id.clone(),
                runtime_address: 0x180001000,
                runtime_module_base: 0x180000000,
                ida_image_base: 0x140000000,
            },
            WorkerRequest::ReverseCoreFunction {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                function: "list_funcs".to_string(),
                arguments: serde_json::json!({}),
                artifact_dir: PathBuf::from("artifacts"),
            },
            WorkerRequest::CloseReverseSession {
                session_id: session_id.clone(),
            },
            WorkerRequest::CloseSession {
                session_id: session_id.clone(),
            },
            WorkerRequest::KillSession {
                session_id: session_id.clone(),
            },
            WorkerRequest::CancelOperation {
                session_id: session_id.clone(),
                operation_id: OperationRef::new(Id::new("op-002").unwrap()),
            },
        ];

        for request in requests {
            assert_eq!(request.session_id(), &session_id);
        }
    }

    #[test]
    fn request_diagnostic_helpers_are_stable() {
        let session_id = SessionRef::new(Id::new("session-001").unwrap());
        let operation_id = OperationRef::new(Id::new("op-001").unwrap());
        let request = WorkerRequest::EvalDebugCommand {
            session_id: session_id.clone(),
            operation_id: operation_id.clone(),
            command: ".echo hi".to_string(),
            artifact_dir: PathBuf::from("artifacts"),
        };

        assert_eq!(request.method_name(), "eval_debug_command");
        assert_eq!(request.session_id(), &session_id);
        assert_eq!(request.operation_id(), Some(&operation_id));

        let request = WorkerRequest::OpenReverseSession {
            session_id,
            ida_install_dir: PathBuf::from("ida"),
            database_path: PathBuf::from("sample.i64"),
        };
        assert_eq!(request.method_name(), "open_reverse_session");
        assert_eq!(request.operation_id(), None);
    }

    #[test]
    fn rejects_wrong_protocol_version() {
        let error = decode_jsonl::<WorkerRequest>(
            r#"{"version":99,"request_id":"req","message":{"method":"close_session","session_id":{"id":"session-001"}}}"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            WorkerProtocolError::UnsupportedVersion {
                actual: 99,
                expected: WORKER_PROTOCOL_VERSION
            }
        ));
    }

    #[test]
    fn huge_json_result_produces_bounded_worker_response() {
        let full = serde_json::json!({
            "function": "huge",
            "pseudocode": "x".repeat(2 * 1024 * 1024),
        });
        let full_len = serde_json::to_vec(&full).unwrap().len() as u64;
        let preview = json_result_preview(&full, full_len, 64 * 1024).unwrap();
        assert!(preview.truncated);
        let response = WorkerEnvelope::new(
            "req-huge",
            WorkerResponse::ReverseCoreFunction {
                result: ReverseCoreFunctionResult {
                    function: "decompile".to_string(),
                    result: preview,
                    warnings: Vec::new(),
                },
                writes: vec![WorkerArtifactWrite {
                    relative_path: PathBuf::from("artifacts/reverse_sessions/s/results/o.json"),
                    kind: "reverse.result".to_string(),
                    role: Some(WorkerArtifactRole::FullResult),
                    byte_len: full_len,
                    description: None,
                }],
            },
        );
        assert!(encode_jsonl(&response).unwrap().len() < 1024 * 1024);
    }

    #[test]
    fn atomic_write_leaves_only_complete_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("results").join("result.json");
        write_atomic_bytes(&path, b"{\"ok\":true}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"ok\":true}");
        assert!(!PathBuf::from(format!("{}.part", path.display())).exists());
    }
}
