use dbgatlas_debug::{DebugCommandResult, DebugMemoryResult, DebugTarget};
use dbgatlas_model::{OperationRef, SessionRef, Timestamp};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

pub const WORKER_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    Failed {
        code: String,
        message: String,
        writes: Vec<WorkerArtifactWrite>,
    },
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
    use dbgatlas_model::{Id, SessionRef};

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
