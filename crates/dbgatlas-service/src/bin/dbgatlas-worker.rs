use dbgatlas_dbgeng::DbgEngSession;
use dbgatlas_debug::{DebugCommandResult, DebugMemoryResult, DebugSessionState, DebugTarget};
use dbgatlas_model::{OperationRef, SessionRef, Timestamp};
use dbgatlas_worker_protocol::{
    ReverseCoreFunctionResult, ReverseFunctionLookupResult, WorkerArtifactWrite, WorkerEnvelope,
    WorkerRequest, WorkerResponse, decode_jsonl, encode_jsonl,
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
        let is_terminal_request = matches!(
            request.message,
            WorkerRequest::CloseSession { .. } | WorkerRequest::KillSession { .. }
        );
        let is_reverse_close = matches!(request.message, WorkerRequest::CloseReverseSession { .. });
        let response = state.handle_request(request.message);
        let should_exit = is_terminal_request
            || (is_reverse_close && matches!(response, WorkerResponse::Ok { .. }));
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
    reverse_session: Option<dbgatlas_ida::IdaSession>,
}

impl WorkerState {
    fn new(expected_session_id: String) -> Self {
        Self {
            expected_session_id,
            session: None,
            reverse_session: None,
        }
    }

    fn handle_request(&mut self, request: WorkerRequest) -> WorkerResponse {
        if request.session_id().id.as_str() != self.expected_session_id {
            return WorkerResponse::failed(
                "session_mismatch",
                format!(
                    "worker session is {}, request was for {}",
                    self.expected_session_id,
                    request.session_id().id.as_str()
                ),
            );
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
            WorkerRequest::OpenReverseSession {
                ida_install_dir,
                database_path,
                ..
            } => self.open_reverse_session(ida_install_dir, database_path),
            WorkerRequest::LookupReverseFunction {
                runtime_address,
                runtime_module_base,
                ida_image_base,
                ..
            } => self.lookup_reverse_function(runtime_address, runtime_module_base, ida_image_base),
            WorkerRequest::ReverseCoreFunction {
                function,
                arguments,
                ..
            } => self.reverse_core_function(function, arguments),
            WorkerRequest::CloseReverseSession { .. } => self.close_reverse_session(),
            WorkerRequest::CloseSession { .. } => {
                self.session = None;
                self.reverse_session = None;
                WorkerResponse::ok("debug session closed")
            }
            WorkerRequest::KillSession { .. } => {
                self.session = None;
                self.reverse_session = None;
                WorkerResponse::ok("debug session killed")
            }
            WorkerRequest::CancelOperation { .. } => {
                WorkerResponse::ok("operation cancel acknowledged")
            }
        }
    }

    fn start_session(&mut self, target: DebugTarget, artifact_dir: &Path) -> WorkerResponse {
        let opened = match target {
            DebugTarget::Dump { path } => DbgEngSession::open_dump(path),
            DebugTarget::Attach { pid } => DbgEngSession::attach(pid),
            DebugTarget::Launch { .. } => {
                return WorkerResponse::failed(
                    "unsupported_target",
                    "launch targets are not supported in MVP1",
                );
            }
        };

        match opened {
            Ok(session) => {
                self.session = Some(session);
                let writes = write_session_event(artifact_dir, &self.expected_session_id, "ready")
                    .map(|write| vec![write])
                    .unwrap_or_default();
                WorkerResponse::ok_with_writes("debug session started", writes)
            }
            Err(error) => WorkerResponse::failed("start_failed", error.to_string()),
        }
    }

    fn open_reverse_session(
        &mut self,
        ida_install_dir: PathBuf,
        database_path: PathBuf,
    ) -> WorkerResponse {
        if self.reverse_session.is_some() {
            return WorkerResponse::failed(
                "reverse_session_exists",
                "reverse session already exists",
            );
        }
        match dbgatlas_ida::IdaSession::open(ida_install_dir, database_path) {
            Ok(session) => {
                self.reverse_session = Some(session);
                WorkerResponse::ReverseSessionOpened { writes: Vec::new() }
            }
            Err(error) => WorkerResponse::failed("reverse_open_failed", error.to_string()),
        }
    }

    fn lookup_reverse_function(
        &mut self,
        runtime_address: u64,
        runtime_module_base: u64,
        ida_image_base: u64,
    ) -> WorkerResponse {
        let Some(session) = self.reverse_session.as_ref() else {
            return WorkerResponse::failed(
                "reverse_session_not_found",
                "reverse session is not open",
            );
        };
        match session.lookup_function(runtime_address, runtime_module_base, ida_image_base) {
            Ok(result) => WorkerResponse::ReverseFunctionLookup {
                result: ReverseFunctionLookupResult {
                    runtime_address: result.runtime_address,
                    runtime_module_base: result.runtime_module_base,
                    rva: result.rva,
                    ida_image_base: result.ida_image_base,
                    ida_ea: result.ida_ea,
                    function_start: result.function_start,
                    function_end: result.function_end,
                    function_name: result.function_name,
                    found: result.found,
                },
                writes: Vec::new(),
            },
            Err(error) => WorkerResponse::failed("reverse_lookup_failed", error.to_string()),
        }
    }

    fn close_reverse_session(&mut self) -> WorkerResponse {
        let Some(session) = self.reverse_session.as_mut() else {
            return WorkerResponse::failed(
                "reverse_session_not_found",
                "reverse session is not open",
            );
        };
        if let Err(error) = session.try_close() {
            return WorkerResponse::failed("reverse_close_failed", error.to_string());
        }
        self.reverse_session = None;
        WorkerResponse::ok("reverse session closed")
    }

    fn reverse_core_function(
        &mut self,
        function: String,
        arguments: serde_json::Value,
    ) -> WorkerResponse {
        let Some(session) = self.reverse_session.as_ref() else {
            return WorkerResponse::failed(
                "reverse_session_not_found",
                "reverse session is not open",
            );
        };
        match session.core_function(function, arguments) {
            Ok(result) => WorkerResponse::ReverseCoreFunction {
                result: ReverseCoreFunctionResult {
                    function: result.function,
                    result: result.result,
                    warnings: result.warnings,
                },
                writes: Vec::new(),
            },
            Err(error) => WorkerResponse::failed("reverse_core_failed", error.to_string()),
        }
    }

    fn with_session<F>(&mut self, f: F) -> WorkerResponse
    where
        F: FnOnce(&DbgEngSession) -> Result<WorkerResponse, std::io::Error>,
    {
        let Some(session) = self.session.as_ref() else {
            return WorkerResponse::failed("session_not_started", "debug session has not started");
        };
        match f(session) {
            Ok(response) => response,
            Err(error) => WorkerResponse::failed("operation_failed", error.to_string()),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use dbgatlas_model::Id;

    fn session_ref(id: &str) -> SessionRef {
        SessionRef::new(Id::new(id).unwrap())
    }

    #[test]
    fn worker_rejects_session_mismatch() {
        let mut state = WorkerState::new("session-expected".to_string());
        let response = state.handle_request(WorkerRequest::CloseSession {
            session_id: session_ref("session-other"),
        });

        match response {
            WorkerResponse::Failed { code, message, .. } => {
                assert_eq!(code, "session_mismatch");
                assert!(message.contains("session-expected"));
            }
            other => panic!("expected failed response, got {other:?}"),
        }
    }

    #[test]
    fn worker_reports_unknown_reverse_session() {
        let mut state = WorkerState::new("session-001".to_string());
        let response = state.handle_request(WorkerRequest::LookupReverseFunction {
            session_id: session_ref("session-001"),
            runtime_address: 0x180001000,
            runtime_module_base: 0x180000000,
            ida_image_base: 0x140000000,
        });

        match response {
            WorkerResponse::Failed { code, message, .. } => {
                assert_eq!(code, "reverse_session_not_found");
                assert!(message.contains("not open"));
            }
            other => panic!("expected failed response, got {other:?}"),
        }
    }

    #[test]
    fn worker_reports_structured_reverse_open_failure() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.i64");
        fs::write(&database, b"sample").unwrap();
        let mut state = WorkerState::new("session-001".to_string());
        let response = state.handle_request(WorkerRequest::OpenReverseSession {
            session_id: session_ref("session-001"),
            ida_install_dir: temp.path().join("missing-ida"),
            database_path: database,
        });

        match response {
            WorkerResponse::Failed { code, message, .. } => {
                assert_eq!(code, "reverse_open_failed");
                assert!(!message.is_empty());
            }
            other => panic!("expected failed response, got {other:?}"),
        }
    }
}
