use dbgatlas_dbgeng::DbgEngSession;
use dbgatlas_debug::{DebugCommandResult, DebugMemoryResult, DebugSessionState, DebugTarget};
use dbgatlas_model::{OperationRef, SessionRef, Timestamp};
use dbgatlas_worker_protocol::{
    WorkerArtifactWrite, WorkerEnvelope, WorkerRequest, WorkerResponse, decode_jsonl, encode_jsonl,
};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

fn main() {
    if let Err(error) = run() {
        eprintln!("dbgatlas-worker error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = WorkerArgs::parse()?;
    let mut pipe = open_pipe(&args.pipe)?;
    let mut state = WorkerState::new(args.session_id.clone());
    loop {
        let line = read_jsonl_line(&mut pipe)?;
        let request: WorkerEnvelope<WorkerRequest> = decode_jsonl(&line)?;
        let should_exit = matches!(
            request.message,
            WorkerRequest::CloseSession { .. } | WorkerRequest::KillSession { .. }
        );
        let response = state.handle_request(request.message);
        let response = WorkerEnvelope::new(request.request_id, response);
        let line = encode_jsonl(&response)?;
        pipe.write_all(line.as_bytes())?;
        pipe.flush()?;
        if should_exit {
            break;
        }
    }
    Ok(())
}

struct WorkerArgs {
    pipe: String,
    session_id: String,
}

impl WorkerArgs {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let mut pipe = None;
        let mut session_id = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--pipe" => pipe = args.next(),
                "--session-id" => session_id = args.next(),
                other => return Err(format!("unsupported argument `{other}`")),
            }
        }
        Ok(Self {
            pipe: pipe.ok_or_else(|| "missing --pipe".to_string())?,
            session_id: session_id.ok_or_else(|| "missing --session-id".to_string())?,
        })
    }
}

struct WorkerState {
    expected_session_id: String,
    session: Option<DbgEngSession>,
}

impl WorkerState {
    fn new(expected_session_id: String) -> Self {
        Self {
            expected_session_id,
            session: None,
        }
    }

    fn handle_request(&mut self, request: WorkerRequest) -> WorkerResponse {
        let session_id = match request_session_id(&request) {
            Some(session_id) => session_id,
            None => {
                return WorkerResponse::Failed {
                    code: "invalid_request".to_string(),
                    message: "request is missing session id".to_string(),
                    writes: Vec::new(),
                };
            }
        };
        if session_id.id.as_str() != self.expected_session_id {
            return WorkerResponse::Failed {
                code: "session_mismatch".to_string(),
                message: format!(
                    "worker session is {}, request was for {}",
                    self.expected_session_id,
                    session_id.id.as_str()
                ),
                writes: Vec::new(),
            };
        }

        match request {
            WorkerRequest::StartDebugSession {
                target,
                artifact_dir,
                ..
            } => self.start_session(target, &artifact_dir),
            WorkerRequest::EvalDebugCommand {
                session_id,
                operation_id,
                command,
                artifact_dir,
            } => self.with_session(|session| {
                let output = session
                    .execute(&command)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                write_command_response(
                    session_id,
                    operation_id,
                    command,
                    output,
                    &artifact_dir,
                    "debug raw command output",
                )
            }),
            WorkerRequest::AddSymbols {
                session_id,
                operation_id,
                symbol_path,
                reload,
                artifact_dir,
            } => self.with_session(|session| {
                let output = session
                    .add_symbols(&symbol_path, reload)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                let command = if reload {
                    format!("add_symbols {symbol_path} --reload")
                } else {
                    format!("add_symbols {symbol_path}")
                };
                write_command_response(
                    session_id,
                    operation_id,
                    command,
                    output,
                    &artifact_dir,
                    "debug add_symbols output",
                )
            }),
            WorkerRequest::ReadMemory {
                session_id,
                operation_id,
                address,
                length,
                artifact_dir,
            } => self.with_session(|session| {
                let length = u32::try_from(length).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "memory read length exceeds u32",
                    )
                })?;
                let bytes = session
                    .read_memory(address, length)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                write_memory_response(
                    session_id,
                    operation_id,
                    address,
                    length,
                    bytes,
                    &artifact_dir,
                )
            }),
            WorkerRequest::CloseSession { .. } => {
                self.session = None;
                WorkerResponse::Ok {
                    summary: "debug session closed".to_string(),
                    writes: Vec::new(),
                }
            }
            WorkerRequest::KillSession { .. } => {
                self.session = None;
                WorkerResponse::Ok {
                    summary: "debug session killed".to_string(),
                    writes: Vec::new(),
                }
            }
            WorkerRequest::CancelOperation { .. } => WorkerResponse::Ok {
                summary: "operation cancel acknowledged".to_string(),
                writes: Vec::new(),
            },
        }
    }

    fn start_session(&mut self, target: DebugTarget, artifact_dir: &Path) -> WorkerResponse {
        let opened = match target {
            DebugTarget::Dump { path } => DbgEngSession::open_dump(path),
            DebugTarget::Attach { pid } => DbgEngSession::attach(pid),
            DebugTarget::Launch { .. } => {
                return WorkerResponse::Failed {
                    code: "unsupported_target".to_string(),
                    message: "launch targets are not supported in MVP1".to_string(),
                    writes: Vec::new(),
                };
            }
        };

        match opened {
            Ok(session) => {
                self.session = Some(session);
                let writes = write_session_event(artifact_dir, &self.expected_session_id, "ready")
                    .map(|write| vec![write])
                    .unwrap_or_default();
                WorkerResponse::Ok {
                    summary: "debug session started".to_string(),
                    writes,
                }
            }
            Err(error) => WorkerResponse::Failed {
                code: "start_failed".to_string(),
                message: error.to_string(),
                writes: Vec::new(),
            },
        }
    }

    fn with_session<F>(&mut self, f: F) -> WorkerResponse
    where
        F: FnOnce(&DbgEngSession) -> Result<WorkerResponse, std::io::Error>,
    {
        let Some(session) = self.session.as_ref() else {
            return WorkerResponse::Failed {
                code: "session_not_started".to_string(),
                message: "debug session has not started".to_string(),
                writes: Vec::new(),
            };
        };
        match f(session) {
            Ok(response) => response,
            Err(error) => WorkerResponse::Failed {
                code: "operation_failed".to_string(),
                message: error.to_string(),
                writes: Vec::new(),
            },
        }
    }
}

fn request_session_id(request: &WorkerRequest) -> Option<&SessionRef> {
    match request {
        WorkerRequest::StartDebugSession { session_id, .. }
        | WorkerRequest::EvalDebugCommand { session_id, .. }
        | WorkerRequest::AddSymbols { session_id, .. }
        | WorkerRequest::ReadMemory { session_id, .. }
        | WorkerRequest::CloseSession { session_id }
        | WorkerRequest::KillSession { session_id }
        | WorkerRequest::CancelOperation { session_id, .. } => Some(session_id),
    }
}

fn write_command_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    command: String,
    output: String,
    artifact_dir: &Path,
    description: &str,
) -> Result<WorkerResponse, std::io::Error> {
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
    fs::write(&raw_path, &output)?;
    let transcript = format!("> {}\n{}\n", command, output);
    append_text(&transcript_path, &transcript)?;
    append_event(
        &events_path,
        &serde_json::json!({
            "event": "output",
            "session_id": session_id,
            "operation_id": operation_id,
            "command": command,
            "timestamp": Timestamp::now(),
            "byte_len": output.len(),
        }),
    )?;

    Ok(WorkerResponse::DebugCommand {
        result: DebugCommandResult {
            session_id,
            operation_id: None,
            command,
            output: output.clone(),
            final_state: Some(DebugSessionState::Ready),
            raw_output: None,
            warnings: Vec::new(),
            error: None,
        },
        writes: vec![
            WorkerArtifactWrite {
                relative_path: raw_relative_path,
                kind: "debug.raw_output".to_string(),
                byte_len: output.len() as u64,
                description: Some(description.to_string()),
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

fn write_memory_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    address: u64,
    requested_length: u32,
    bytes: Vec<u8>,
    artifact_dir: &Path,
) -> Result<WorkerResponse, std::io::Error> {
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
    fs::write(&memory_path, &bytes)?;

    Ok(WorkerResponse::DebugMemory {
        result: DebugMemoryResult {
            session_id,
            operation_id: None,
            address,
            requested_length: requested_length as u64,
            bytes_read: bytes.len() as u64,
            memory: None,
            warnings: Vec::new(),
            error: None,
        },
        writes: vec![WorkerArtifactWrite {
            relative_path,
            kind: "debug.memory".to_string(),
            byte_len: bytes.len() as u64,
            description: Some("debug memory read".to_string()),
        }],
    })
}

fn write_session_event(
    artifact_dir: &Path,
    session_id: &str,
    state: &str,
) -> Result<WorkerArtifactWrite, std::io::Error> {
    let events_path = artifact_dir.join("events.jsonl");
    append_event(
        &events_path,
        &serde_json::json!({
            "event": "state_changed",
            "session_id": { "id": session_id },
            "state": state,
            "timestamp": Timestamp::now(),
        }),
    )?;
    Ok(WorkerArtifactWrite {
        relative_path: PathBuf::from("artifacts")
            .join("sessions")
            .join(session_id)
            .join("events.jsonl"),
        kind: "debug.events".to_string(),
        byte_len: 0,
        description: Some("debug session events".to_string()),
    })
}

fn session_relative_path(session_id: &SessionRef, suffix: &str) -> PathBuf {
    PathBuf::from("artifacts")
        .join("sessions")
        .join(session_id.id.as_str())
        .join(suffix)
}

fn append_text(path: &Path, text: &str) -> Result<(), std::io::Error> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(text.as_bytes())
}

fn append_event(path: &Path, event: &serde_json::Value) -> Result<(), std::io::Error> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, event)?;
    file.write_all(b"\n")
}

fn read_jsonl_line(reader: &mut impl Read) -> Result<String, std::io::Error> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let read = reader.read(&mut byte)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "pipe closed before request",
            ));
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() > 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request line is too large",
            ));
        }
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(windows)]
fn open_pipe(pipe_name: &str) -> Result<std::fs::File, std::io::Error> {
    let mut last_error = None;
    for _ in 0..50 {
        match OpenOptions::new().read(true).write(true).open(pipe_name) {
            Ok(file) => return Ok(file),
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out opening pipe")
    }))
}

#[cfg(not(windows))]
fn open_pipe(_pipe_name: &str) -> Result<std::fs::File, std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "named pipe worker transport is only supported on Windows",
    ))
}
