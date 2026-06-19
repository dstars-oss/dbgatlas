use dbgatlas_debug::{DebugCommandResult, DebugMemoryResult, DebugTarget};
use dbgatlas_model::{OperationRef, SessionRef, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use thiserror::Error;

pub const WORKER_PROTOCOL_VERSION: u32 = 2;

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
        artifact_dir: PathBuf,
    },
    LookupReverseFunction {
        session_id: SessionRef,
        operation_id: OperationRef,
        runtime_address: u64,
        runtime_module_base: u64,
        ida_image_base: u64,
        artifact_dir: PathBuf,
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
    pub result: Value,
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
    pub byte_len: u64,
    pub description: Option<String>,
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
                artifact_dir: PathBuf::from(
                    r"C:\case\dbgatlas\artifacts\reverse_sessions\session-001",
                ),
            },
        );
        let lookup = WorkerEnvelope::new(
            "req-rev-lookup",
            WorkerRequest::LookupReverseFunction {
                session_id: session_id.clone(),
                operation_id: OperationRef::new(Id::new("op-001").unwrap()),
                runtime_address: 0x180001234,
                runtime_module_base: 0x180000000,
                ida_image_base: 0x140000000,
                artifact_dir: PathBuf::from(
                    r"C:\case\dbgatlas\artifacts\reverse_sessions\session-001",
                ),
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
                artifact_dir: PathBuf::from(
                    r"C:\case\dbgatlas\artifacts\reverse_sessions\session-001",
                ),
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
                    function: "imports".to_string(),
                    result: serde_json::json!({
                        "offset": 0,
                        "count": 1,
                        "items": [{
                            "module": "KERNEL32.dll",
                            "name": "CreateFileW",
                            "ordinal": null
                        }]
                    }),
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
                target: DebugTarget::Dump {
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
                artifact_dir: PathBuf::from("artifacts"),
            },
            WorkerRequest::LookupReverseFunction {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
                runtime_address: 0x180001000,
                runtime_module_base: 0x180000000,
                ida_image_base: 0x140000000,
                artifact_dir: PathBuf::from("artifacts"),
            },
            WorkerRequest::ReverseCoreFunction {
                session_id: session_id.clone(),
                operation_id,
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
}
