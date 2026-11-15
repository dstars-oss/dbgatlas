use dbgatlas_debug::{DebugCommandResult, DebugMemoryResult, DebugTarget};
use dbgatlas_model::{OperationRef, SessionRef, Timestamp};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

pub const WORKER_PROTOCOL_VERSION: u32 = 9;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerSnapshot {
    #[serde(default)]
    pub sequence: u64,
    pub target_process: Option<dbgatlas_model::ProcessInstance>,
    pub target_state: dbgatlas_model::TargetState,
    pub observed_at: Timestamp,
    pub context: serde_json::Value,
}

/// Business responses and unsolicited observations share a single reader. Control
/// requests use another pipe and never queue behind a business response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerMessage {
    Response { result: WorkerResponse },
    Event { events: Vec<serde_json::Value> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerControlRequest {
    pub session_id: SessionRef,
    #[serde(flatten)]
    pub action: WorkerControlAction,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorkerControlAction {
    Interrupt { expected_epoch: u64 },
    Cancel { operation_id: OperationRef },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerControlResponse {
    pub accepted: bool,
    pub code: String,
    pub epoch: Option<u64>,
    pub native_hresult: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerEnvelope<T> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<WorkerSnapshot>,
    pub version: u32,
    pub request_id: String,
    pub message: T,
}

impl<T> WorkerEnvelope<T> {
    pub fn new(request_id: impl Into<String>, message: T) -> Self {
        Self {
            snapshot: None,
            version: WORKER_PROTOCOL_VERSION,
            request_id: request_id.into(),
            message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum WorkerRequest {
    Recording {
        session_id: SessionRef,
        operation_id: OperationRef,
        action: String,
        params: serde_json::Value,
        ttd_dir: Option<PathBuf>,
    },
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
            WorkerRequest::Recording { .. } => "recording",
            WorkerRequest::StartDebugSession { .. } => "start_debug_session",
            WorkerRequest::EvalDebugCommand { .. } => "eval_debug_command",
            WorkerRequest::AddSymbols { .. } => "add_symbols",
            WorkerRequest::ReadMemory { .. } => "read_memory",
            WorkerRequest::CloseSession { .. } => "close_session",
            WorkerRequest::KillSession { .. } => "kill_session",
            WorkerRequest::CancelOperation { .. } => "cancel_operation",
        }
    }

    pub fn session_id(&self) -> &SessionRef {
        match self {
            WorkerRequest::Recording { session_id, .. } => session_id,
            WorkerRequest::StartDebugSession { session_id, .. }
            | WorkerRequest::EvalDebugCommand { session_id, .. }
            | WorkerRequest::AddSymbols { session_id, .. }
            | WorkerRequest::ReadMemory { session_id, .. }
            | WorkerRequest::CloseSession { session_id }
            | WorkerRequest::KillSession { session_id }
            | WorkerRequest::CancelOperation { session_id, .. } => session_id,
        }
    }

    pub fn operation_id(&self) -> Option<&OperationRef> {
        match self {
            WorkerRequest::Recording { operation_id, .. } => Some(operation_id),
            WorkerRequest::EvalDebugCommand { operation_id, .. }
            | WorkerRequest::AddSymbols { operation_id, .. }
            | WorkerRequest::ReadMemory { operation_id, .. }
            | WorkerRequest::CancelOperation { operation_id, .. } => Some(operation_id),
            WorkerRequest::StartDebugSession { .. }
            | WorkerRequest::CloseSession { .. }
            | WorkerRequest::KillSession { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerResponse {
    Recording {
        result: serde_json::Value,
        writes: Vec<WorkerArtifactWrite>,
    },
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
            WorkerResponse::Recording { writes, .. } => writes,
            WorkerResponse::Ok { writes, .. }
            | WorkerResponse::DebugCommand { writes, .. }
            | WorkerResponse::DebugMemory { writes, .. }
            | WorkerResponse::Failed { writes, .. } => writes,
        }
    }
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

        let request = WorkerRequest::CloseSession { session_id };
        assert_eq!(request.method_name(), "close_session");
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
    fn atomic_write_leaves_only_complete_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("results").join("result.json");
        write_atomic_bytes(&path, b"{\"ok\":true}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"ok\":true}");
        assert!(!PathBuf::from(format!("{}.part", path.display())).exists());
    }
}
