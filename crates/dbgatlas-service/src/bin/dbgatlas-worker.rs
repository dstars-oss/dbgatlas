#![cfg_attr(windows, windows_subsystem = "windows")]

use dbgatlas_dbgeng::DbgEngSession;
use dbgatlas_debug::{DebugCommandResult, DebugMemoryResult, DebugSessionState, DebugTarget};
use dbgatlas_model::{
    DEFAULT_INLINE_RESULT_BYTE_LIMIT, OperationRef, ResultPreview, SessionRef, Timestamp,
};
use dbgatlas_runtime::RuntimeConfig;
use dbgatlas_worker_protocol::{
    ReverseCoreFunctionResult, ReverseFunctionLookupResult, ReverseRuntimeInfo, WorkerArtifactRole,
    WorkerArtifactWrite, WorkerEnvelope, WorkerRequest, WorkerResponse, decode_jsonl, encode_jsonl,
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
    let args = WorkerArgs::parse()?;
    let mut pipe = open_pipe(&args.pipe)?;
    let lifetime_pipe = open_pipe(&args.lifetime_pipe)?;
    let _watchdog = start_lifetime_watchdog(lifetime_pipe);
    let mut state = WorkerState::new(args.session_id.clone(), args.dbgeng_dirs);
    loop {
        // worker 与 service 之间只走一行一个 JSON envelope 的 pipe 协议。
        // 运行诊断由 service 侧记录；worker 只负责执行 native capability 并回传 artifact 写入声明。
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

fn start_lifetime_watchdog(mut lifetime_pipe: std::fs::File) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        loop {
            match read_lifetime_line_with_timeout(&mut lifetime_pipe, LIFETIME_WATCHDOG_TIMEOUT) {
                Ok(line) if line.trim_end() == "tick" => {}
                Ok(_) => {}
                Err(_) => {
                    terminate_descendant_processes(std::process::id());
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

#[cfg(windows)]
fn terminate_descendant_processes(root_pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    for _ in 0..3 {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return;
        }
        let mut processes = Vec::new();
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) };
        while ok != 0 {
            processes.push((entry.th32ProcessID, entry.th32ParentProcessID));
            ok = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        unsafe {
            CloseHandle(snapshot);
        }

        let mut descendants = Vec::new();
        let mut frontier = vec![root_pid];
        while let Some(parent_pid) = frontier.pop() {
            for (pid, ppid) in &processes {
                if *ppid == parent_pid && *pid != root_pid && !descendants.contains(pid) {
                    descendants.push(*pid);
                    frontier.push(*pid);
                }
            }
        }

        if descendants.is_empty() {
            return;
        }

        for pid in descendants.into_iter().rev() {
            let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
            if handle.is_null() {
                continue;
            }
            unsafe {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(windows))]
fn terminate_descendant_processes(_root_pid: u32) {}

struct WorkerState {
    expected_session_id: String,
    dbgeng_dirs: Vec<PathBuf>,
    session: Option<DbgEngSession>,
    reverse_session: Option<dbgatlas_ida::IdaSession>,
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
            expected_session_id,
            dbgeng_dirs,
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
                let raw_path = artifact_dir
                    .join("raw")
                    .join(format!("{}.txt", operation_id.id.as_str()));
                let captured = session
                    .execute_to_artifact(&command, &raw_path, DEFAULT_INLINE_RESULT_BYTE_LIMIT)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                write_captured_command_response(
                    session_id,
                    operation_id,
                    command,
                    captured,
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
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
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
                operation_id,
                function,
                arguments,
                artifact_dir,
                ..
            } => self.reverse_core_function(operation_id, function, arguments, &artifact_dir),
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
                // 真实的强制取消由 service supervisor 负责；worker 能收到该请求时，
                // 说明 pipe 空闲，返回 ack 让上层把 operation 标记为 canceled 即可。
                WorkerResponse::ok("operation cancel acknowledged")
            }
        }
    }

    fn start_session(&mut self, target: DebugTarget, artifact_dir: &Path) -> WorkerResponse {
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
                let info = session.runtime_info();
                let runtime = ReverseRuntimeInfo {
                    runtime_major: info.runtime_major,
                    runtime_minor: info.runtime_minor,
                    runtime_build: info.runtime_build,
                    provider_file_name: info.provider_file_name.clone(),
                    ida_sdk_version: info.ida_sdk_version,
                    abi_major: info.abi_major,
                    abi_minor: info.abi_minor,
                    abi_patch: info.abi_patch,
                };
                self.reverse_session = Some(session);
                WorkerResponse::ReverseSessionOpened {
                    runtime,
                    writes: Vec::new(),
                }
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
        operation_id: OperationRef,
        function: String,
        arguments: serde_json::Value,
        artifact_dir: &Path,
    ) -> WorkerResponse {
        let Some(session) = self.reverse_session.as_ref() else {
            return WorkerResponse::failed(
                "reverse_session_not_found",
                "reverse session is not open",
            );
        };
        let result_path = artifact_dir
            .join("results")
            .join(format!("{}.json", operation_id.id.as_str()));
        match session.core_function_to_artifact(
            function,
            arguments,
            &result_path,
            DEFAULT_INLINE_RESULT_BYTE_LIMIT,
        ) {
            Ok(result) => {
                let actual_len = match fs::metadata(&result_path) {
                    Ok(metadata) => metadata.len(),
                    Err(error) => {
                        return WorkerResponse::failed(
                            "reverse_core_artifact_failed",
                            error.to_string(),
                        );
                    }
                };
                if actual_len != result.full_byte_len {
                    return WorkerResponse::failed(
                        "reverse_core_artifact_failed",
                        format!(
                            "native artifact length mismatch: reported {}, actual {actual_len}",
                            result.full_byte_len
                        ),
                    );
                }
                let preview = ResultPreview {
                    value: result.preview,
                    truncated: result.truncated,
                    full_byte_len: result.full_byte_len,
                    inline_byte_limit: result.inline_byte_limit,
                };
                WorkerResponse::ReverseCoreFunction {
                    result: ReverseCoreFunctionResult {
                        function: result.function,
                        result: preview,
                        warnings: result.warnings,
                    },
                    writes: vec![WorkerArtifactWrite {
                        relative_path: PathBuf::from("artifacts")
                            .join("reverse_sessions")
                            .join(&self.expected_session_id)
                            .join("results")
                            .join(format!("{}.json", operation_id.id.as_str())),
                        kind: "reverse.result".to_string(),
                        role: Some(WorkerArtifactRole::FullResult),
                        byte_len: result.full_byte_len,
                        description: Some("complete IDA Core Function result".to_string()),
                    }],
                }
            }
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
    fn worker_reports_unknown_reverse_session() {
        let mut state = WorkerState::new("session-001".to_string(), Vec::new());
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
        let mut state = WorkerState::new("session-001".to_string(), Vec::new());
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
