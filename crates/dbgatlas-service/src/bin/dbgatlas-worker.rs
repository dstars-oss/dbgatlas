#![cfg_attr(windows, windows_subsystem = "windows")]

use dbgatlas_dbgeng::DbgEngSession;
use dbgatlas_debug::{DebugCommandResult, DebugMemoryResult, DebugSessionState, DebugTarget};
use dbgatlas_model::{
    DEFAULT_INLINE_RESULT_BYTE_LIMIT, OperationRef, ResultPreview, SessionRef, Timestamp,
};
use dbgatlas_runtime::RuntimeConfig;
use dbgatlas_worker_protocol::{
    WorkerArtifactRole, WorkerArtifactWrite, WorkerEnvelope, WorkerRequest, WorkerResponse,
    decode_jsonl, encode_jsonl,
};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const LIFETIME_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(10);

fn main() {
    if let Err(error) = run() {
        eprintln!("dbgatlas-worker error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut special = std::env::args_os().skip(1);
    if special.next().as_deref() == Some(std::ffi::OsStr::new("--cleanup-recording")) {
        let directory = special
            .next()
            .ok_or("missing recording cleanup directory")?;
        dbgatlas_service::recover_recording_resources(Path::new(&directory))?;
        return Ok(());
    }
    let args = WorkerArgs::parse()?;
    let mut pipe = open_pipe(&args.pipe)?;
    let lifetime_pipe = open_pipe(&args.lifetime_pipe)?;
    let ownership_file = std::sync::Arc::new(std::sync::Mutex::new(None));
    let _watchdog = start_lifetime_watchdog(lifetime_pipe, ownership_file.clone());
    let mut state = WorkerState::new(args.session_id.clone(), args.dbgeng_dirs);
    state.ownership_file = ownership_file;
    loop {
        // worker 与 service 之间只走一行一个 JSON envelope 的 pipe 协议。
        // 运行诊断由 service 侧记录；worker 只负责执行 native capability 并回传 artifact 写入声明。
        let line = read_jsonl_line(&mut pipe)?;
        let request: WorkerEnvelope<WorkerRequest> = decode_jsonl(&line)?;
        let is_terminal_request = matches!(
            request.message,
            WorkerRequest::CloseSession { .. } | WorkerRequest::KillSession { .. }
        );
        let response = state.handle_request(request.message);
        let should_exit = is_terminal_request && matches!(response, WorkerResponse::Ok { .. });
        let mut response = WorkerEnvelope::new(request.request_id, response);
        response.snapshot = state.snapshot();
        let line = encode_jsonl(&response)?;
        pipe.write_all(line.as_bytes())?;
        pipe.flush()?;
        if should_exit {
            break;
        }
    }
    Ok(())
}

#[derive(Debug)]
struct WorkerArgs {
    pipe: String,
    lifetime_pipe: String,
    session_id: String,
    dbgeng_dirs: Vec<PathBuf>,
}

impl WorkerArgs {
    fn parse() -> Result<Self, String> {
        Self::parse_from(std::env::args().skip(1))
    }

    fn parse_from<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = String>,
    {
        let mut args = args.into_iter();
        let mut pipe = None;
        let mut lifetime_pipe = None;
        let mut session_id = None;
        let mut dbgeng_dirs = Vec::new();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--pipe" => pipe = args.next(),
                "--lifetime-pipe" => lifetime_pipe = args.next(),
                "--session-id" => session_id = args.next(),
                "--dbgeng-dir" => {
                    dbgeng_dirs.push(
                        args.next()
                            .map(PathBuf::from)
                            .ok_or_else(|| "missing --dbgeng-dir value".to_string())?,
                    );
                }
                other => return Err(format!("unsupported argument `{other}`")),
            }
        }
        Ok(Self {
            pipe: pipe.ok_or_else(|| "missing --pipe".to_string())?,
            lifetime_pipe: lifetime_pipe.ok_or_else(|| "missing --lifetime-pipe".to_string())?,
            session_id: session_id.ok_or_else(|| "missing --session-id".to_string())?,
            dbgeng_dirs,
        })
    }
}

fn start_lifetime_watchdog(
    mut lifetime_pipe: std::fs::File,
    ownership_file: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        loop {
            match read_lifetime_line_with_timeout(&mut lifetime_pipe, LIFETIME_WATCHDOG_TIMEOUT) {
                Ok(line) if line.trim_end() == "tick" => {}
                Ok(_) => {}
                Err(_) => {
                    if let Ok(path) = ownership_file.lock() {
                        if let Some(path) = path.as_ref() {
                            let _ = dbgatlas_service::process_identity::cleanup(path);
                            if let Some(directory) = path.parent() {
                                let _ = dbgatlas_service::recover_recording_resources(directory);
                            }
                        }
                    }
                    std::process::exit(0);
                }
            }
        }
    })
}

#[cfg(windows)]
fn read_lifetime_line_with_timeout(
    reader: &mut std::fs::File,
    timeout: Duration,
) -> Result<String, std::io::Error> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let started = Instant::now();
    let mut bytes = Vec::new();
    loop {
        let mut available = 0;
        let ok = unsafe {
            PeekNamedPipe(
                reader.as_raw_handle() as _,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if available == 0 {
            if started.elapsed() >= timeout {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for lifetime heartbeat",
                ));
            }
            thread::sleep(Duration::from_millis(25));
            continue;
        }
        let mut byte = [0u8; 1];
        let read = reader.read(&mut byte)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "lifetime pipe closed",
            ));
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return String::from_utf8(bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
        if bytes.len() > 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "lifetime heartbeat line is too large",
            ));
        }
    }
}

#[cfg(not(windows))]
fn read_lifetime_line_with_timeout(
    reader: &mut std::fs::File,
    _timeout: Duration,
) -> Result<String, std::io::Error> {
    read_jsonl_line(reader)
}

struct WorkerState {
    recording: Option<dbgatlas_service::RecordingExecutor>,
    ownership_file: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
    file_target: bool,
    normal_exit: bool,
    expected_session_id: String,
    dbgeng_dirs: Vec<PathBuf>,
    session: Option<DbgEngSession>,
}

impl Drop for WorkerState {
    fn drop(&mut self) {
        if self.normal_exit {
            return;
        }
        // Detach before waiting for owned targets to exit: a pending debug event can otherwise hold termination.
        self.session.take();
        if let Ok(path) = self.ownership_file.lock() {
            if let Some(path) = path.as_ref() {
                if let Err(error) = dbgatlas_service::process_identity::cleanup(path) {
                    eprintln!("abnormal worker cleanup: {error}");
                }
                if let Some(directory) = path.parent() {
                    if let Err(error) = dbgatlas_service::recover_recording_resources(directory) {
                        eprintln!("abnormal recording cleanup: {error}");
                    }
                }
            }
        }
    }
}

impl WorkerState {
    fn new(expected_session_id: String, mut dbgeng_dirs: Vec<PathBuf>) -> Self {
        if dbgeng_dirs.is_empty() {
            dbgeng_dirs = RuntimeConfig::default()
                .resolve_tool_paths()
                .dbgeng_candidates
                .into_iter()
                .map(|location| location.dir)
                .collect();
        }
        Self {
            recording: None,
            ownership_file: std::sync::Arc::new(std::sync::Mutex::new(None)),
            file_target: false,
            normal_exit: false,
            expected_session_id,
            dbgeng_dirs,
            session: None,
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
            WorkerRequest::Recording {
                session_id,
                operation_id,
                action,
                params,
                ttd_dir,
            } => {
                if self.session.is_some() {
                    return WorkerResponse::failed(
                        "backend_mismatch",
                        "worker already bound to debug",
                    );
                }
                if let Some(root) = params
                    .get("project_root")
                    .and_then(serde_json::Value::as_str)
                {
                    *self.ownership_file.lock().unwrap() = Some(
                        PathBuf::from(root)
                            .join("dbgatlas/artifacts/recordings")
                            .join(session_id.id.as_str())
                            .join("owned-processes.json"),
                    );
                }
                let executor = self.recording.get_or_insert_with(|| {
                    dbgatlas_service::RecordingExecutor::new(session_id, ttd_dir)
                });
                executor.execute(operation_id, &action, params)
            }
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
                let raw_path = artifact_dir
                    .join("raw")
                    .join(format!("{}.txt", operation_id.id.as_str()));
                let captured = session
                    .execute_to_artifact(&command, &raw_path, DEFAULT_INLINE_RESULT_BYTE_LIMIT)
                    .map_err(std::io::Error::other)?;
                write_captured_command_response(
                    session_id,
                    operation_id,
                    command,
                    captured,
                    &artifact_dir,
                    "debug raw command output",
                )
                .map_err(|e| {
                    std::io::Error::other(dbgatlas_dbgeng::DbgEngError::OutputAfterExecution(
                        e.to_string(),
                    ))
                })
            }),
            WorkerRequest::AddSymbols {
                session_id,
                operation_id,
                symbol_path,
                reload,
                artifact_dir,
            } => self.with_session(|session| {
                let raw_path = artifact_dir
                    .join("raw")
                    .join(format!("{}.txt", operation_id.id.as_str()));
                let captured = session
                    .add_symbols_to_artifact(
                        &symbol_path,
                        reload,
                        &raw_path,
                        DEFAULT_INLINE_RESULT_BYTE_LIMIT,
                    )
                    .map_err(std::io::Error::other)?;
                let command = if reload {
                    format!("add_symbols {symbol_path} --reload")
                } else {
                    format!("add_symbols {symbol_path}")
                };
                write_captured_command_response(
                    session_id,
                    operation_id,
                    command,
                    captured,
                    &artifact_dir,
                    "debug add_symbols output",
                )
                .map_err(|e| {
                    std::io::Error::other(dbgatlas_dbgeng::DbgEngError::OutputAfterExecution(
                        e.to_string(),
                    ))
                })
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
                    .map_err(std::io::Error::other)?;
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
                if let Ok(path) = self.ownership_file.lock() {
                    if let Some(path) = path.as_ref() {
                        if let Err(error) =
                            dbgatlas_service::process_identity::release_targets(path)
                        {
                            return WorkerResponse::failed(
                                "target_release_failed",
                                error.to_string(),
                            );
                        }
                    }
                }
                if let Some(session) = self.session.as_mut() {
                    if let Err(error) = session.close() {
                        return WorkerResponse::failed("detach_failed", error.to_string());
                    }
                }
                self.session = None;
                self.normal_exit = true;
                WorkerResponse::ok("debug session closed")
            }
            WorkerRequest::KillSession { .. } => {
                self.session = None;
                WorkerResponse::ok("debug session killed")
            }
            WorkerRequest::CancelOperation { .. } => {
                // 真实的强制取消由 service supervisor 负责；worker 能收到该请求时，
                // 说明 pipe 空闲，返回 ack 让上层把 operation 标记为 canceled 即可。
                WorkerResponse::ok("operation cancel acknowledged")
            }
        }
    }

    fn snapshot(&self) -> Option<dbgatlas_worker_protocol::WorkerSnapshot> {
        let session = self.session.as_ref()?;
        let engine = session.engine_state().ok()?;
        let (status, pid) = (engine.target_state, engine.process_id);
        if status == 3 {
            if let Ok(path) = self.ownership_file.lock() {
                if let Some(path) = path.as_ref() {
                    let _ = dbgatlas_service::process_identity::release_targets(path);
                }
            }
        }
        use dbgatlas_model::TargetState;
        Some(dbgatlas_worker_protocol::WorkerSnapshot {
            target_process: if self.file_target || pid == 0 {
                None
            } else {
                dbgatlas_service::process_identity::inspect(pid).ok()
            },
            target_state: if self.file_target {
                TargetState::Offline
            } else {
                match status {
                    1 => TargetState::Running,
                    2 => TargetState::Stopped,
                    _ => TargetState::Unknown,
                }
            },
            observed_at: Timestamp::now(),
            context: serde_json::json!({"thread_id":engine.thread_id,"frame_index":engine.frame_index}),
        })
    }

    fn start_session(&mut self, target: DebugTarget, artifact_dir: &Path) -> WorkerResponse {
        if self.session.is_some() || self.recording.is_some() {
            return WorkerResponse::failed("already_open", "worker is already bound");
        }
        self.file_target = matches!(target, DebugTarget::File { .. });
        let created = matches!(target, DebugTarget::Launch { .. });
        let ownership_path = artifact_dir.join("owned-processes.json");
        *self.ownership_file.lock().unwrap() = Some(ownership_path.clone());
        if created {
            if let Err(error) = dbgatlas_worker_protocol::write_atomic_bytes(
                &artifact_dir.join("unconfirmed-launch.json"),
                b"{\"ownership\":\"unconfirmed\"}",
            ) {
                return WorkerResponse::failed("launch_intent_failed", error.to_string());
            }
        }
        let opened = match target {
            DebugTarget::File { path } => {
                DbgEngSession::open_file_with_runtimes(path, &self.dbgeng_dirs)
            }
            DebugTarget::Attach { pid } => {
                DbgEngSession::attach_with_runtimes(pid, &self.dbgeng_dirs)
            }
            DebugTarget::Launch { executable, args } => {
                DbgEngSession::launch_with_runtimes(executable, &args, &self.dbgeng_dirs)
            }
        };

        match opened {
            Ok(session) => {
                if created {
                    let result = session
                        .state()
                        .map_err(|e| e.to_string())
                        .and_then(|(_, pid)| {
                            dbgatlas_service::process_identity::register(
                                &ownership_path,
                                pid,
                                "target",
                            )
                            .map_err(|e| e.to_string())
                        });
                    if let Err(error) = result {
                        return WorkerResponse::failed("ownership_registration_failed", error);
                    }
                    if let Err(error) =
                        std::fs::remove_file(artifact_dir.join("unconfirmed-launch.json"))
                    {
                        return WorkerResponse::failed(
                            "ownership_commit_failed",
                            error.to_string(),
                        );
                    }
                    if let Err(error) = session.commit_launch() {
                        return WorkerResponse::failed("launch_commit_failed", error.to_string());
                    }
                }
                self.session = Some(session);
                let writes = write_session_event(artifact_dir, &self.expected_session_id, "ready")
                    .map(|write| vec![write])
                    .unwrap_or_default();
                WorkerResponse::ok_with_writes("debug session started", writes)
            }
            Err(error) => WorkerResponse::failed("start_failed", error.to_string()),
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
            Err(error) => {
                let code = match error
                    .get_ref()
                    .and_then(|e| e.downcast_ref::<dbgatlas_dbgeng::DbgEngError>())
                {
                    Some(dbgatlas_dbgeng::DbgEngError::OutputAfterExecution(_))
                    | Some(dbgatlas_dbgeng::DbgEngError::Native { status: 501, .. }) => {
                        "execution_succeeded_output_failed"
                    }
                    Some(dbgatlas_dbgeng::DbgEngError::Native { status: 502, .. }) => {
                        "execution_failed_output_failed"
                    }
                    _ => "operation_failed",
                };
                WorkerResponse::failed(code, error.to_string())
            }
        }
    }
}

fn write_captured_command_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    command: String,
    captured: dbgatlas_dbgeng::ArtifactCommandOutput,
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
    let mut transcript = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript_path)?;
    let prefix = format!("> {}\n", command);
    transcript.write_all(prefix.as_bytes())?;
    let copied = std::io::copy(&mut fs::File::open(&raw_path)?, &mut transcript)?;
    transcript.write_all(b"\n")?;
    append_event(
        &events_path,
        &serde_json::json!({
            "event": "output",
            "session_id": session_id,
            "operation_id": operation_id,
            "command": command,
            "timestamp": Timestamp::now(),
            "byte_len": captured.full_byte_len,
        }),
    )?;
    let bounded = ResultPreview::utf8(&captured.preview, DEFAULT_INLINE_RESULT_BYTE_LIMIT);
    let preview_byte_len = bounded.value.len() as u64;
    let preview = ResultPreview {
        value: bounded.value,
        truncated: captured.truncated || captured.full_byte_len > preview_byte_len,
        full_byte_len: captured.full_byte_len,
        inline_byte_limit: captured.inline_byte_limit,
    };
    Ok(WorkerResponse::DebugCommand {
        result: DebugCommandResult {
            session_id,
            operation_id: None,
            command,
            result: preview,
            final_state: Some(DebugSessionState::Ready),
            warnings: Vec::new(),
            error: None,
        },
        writes: vec![
            WorkerArtifactWrite {
                relative_path: raw_relative_path,
                kind: "debug.raw_output".to_string(),
                role: Some(WorkerArtifactRole::FullResult),
                byte_len: captured.full_byte_len,
                description: Some(description.to_string()),
            },
            WorkerArtifactWrite {
                relative_path: transcript_relative_path,
                kind: "debug.transcript".to_string(),
                role: None,
                byte_len: prefix.len() as u64 + copied + 1,
                description: Some("debug session transcript".to_string()),
            },
            WorkerArtifactWrite {
                relative_path: events_relative_path,
                kind: "debug.events".to_string(),
                role: None,
                byte_len: captured.full_byte_len,
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
            role: None,
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
        role: None,
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
        let mut state = WorkerState::new("session-expected".to_string(), Vec::new());
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
    fn worker_args_require_lifetime_pipe() {
        let error = WorkerArgs::parse_from(vec![
            "--pipe".to_string(),
            r"\\.\pipe\dbgatlas-rpc-test".to_string(),
            "--session-id".to_string(),
            "session-001".to_string(),
        ])
        .unwrap_err();

        assert!(error.contains("missing --lifetime-pipe"));
    }

    #[test]
    fn worker_args_accept_lifetime_pipe() {
        let args = WorkerArgs::parse_from(vec![
            "--pipe".to_string(),
            r"\\.\pipe\dbgatlas-rpc-test".to_string(),
            "--lifetime-pipe".to_string(),
            r"\\.\pipe\dbgatlas-life-test".to_string(),
            "--session-id".to_string(),
            "session-001".to_string(),
        ])
        .unwrap();

        assert_eq!(args.pipe, r"\\.\pipe\dbgatlas-rpc-test");
        assert_eq!(args.lifetime_pipe, r"\\.\pipe\dbgatlas-life-test");
        assert_eq!(args.session_id, "session-001");
    }
}
