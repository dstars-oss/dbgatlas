use dbgatlas_debug::{DebugCommandResult, DebugSessionState};
use dbgatlas_model::{OperationRef, SessionRef};
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
    loop {
        let line = read_jsonl_line(&mut pipe)?;
        let request: WorkerEnvelope<WorkerRequest> = decode_jsonl(&line)?;
        let should_exit = matches!(
            request.message,
            WorkerRequest::CloseSession { .. } | WorkerRequest::KillSession { .. }
        );
        let response = handle_request(&args.session_id, request.message);
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

fn handle_request(expected_session_id: &str, request: WorkerRequest) -> WorkerResponse {
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
    if session_id.id.as_str() != expected_session_id {
        return WorkerResponse::Failed {
            code: "session_mismatch".to_string(),
            message: format!(
                "worker session is {expected_session_id}, request was for {}",
                session_id.id.as_str()
            ),
            writes: Vec::new(),
        };
    }

    match request {
        WorkerRequest::StartDebugSession { .. } => WorkerResponse::Ok {
            summary: "debug session started".to_string(),
            writes: Vec::new(),
        },
        WorkerRequest::EvalDebugCommand {
            session_id,
            operation_id,
            command,
            artifact_dir,
        } => match write_eval_response(session_id, operation_id, command, &artifact_dir) {
            Ok(response) => response,
            Err(error) => WorkerResponse::Failed {
                code: "eval_failed".to_string(),
                message: error.to_string(),
                writes: Vec::new(),
            },
        },
        WorkerRequest::CloseSession { .. } => WorkerResponse::Ok {
            summary: "debug session closed".to_string(),
            writes: Vec::new(),
        },
        WorkerRequest::KillSession { .. } => WorkerResponse::Ok {
            summary: "debug session killed".to_string(),
            writes: Vec::new(),
        },
        WorkerRequest::CancelOperation { .. } => WorkerResponse::Ok {
            summary: "operation cancel acknowledged".to_string(),
            writes: Vec::new(),
        },
    }
}

fn request_session_id(request: &WorkerRequest) -> Option<&SessionRef> {
    match request {
        WorkerRequest::StartDebugSession { session_id, .. }
        | WorkerRequest::EvalDebugCommand { session_id, .. }
        | WorkerRequest::CloseSession { session_id }
        | WorkerRequest::KillSession { session_id }
        | WorkerRequest::CancelOperation { session_id, .. } => Some(session_id),
    }
}

fn write_eval_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    command: String,
    artifact_dir: &Path,
) -> Result<WorkerResponse, std::io::Error> {
    let raw_relative_path = PathBuf::from("artifacts")
        .join("sessions")
        .join(session_id.id.as_str())
        .join("raw")
        .join(format!("{}.txt", operation_id.id.as_str()));
    let transcript_relative_path = PathBuf::from("artifacts")
        .join("sessions")
        .join(session_id.id.as_str())
        .join("transcript.log");
    let raw_path = artifact_dir
        .join("raw")
        .join(format!("{}.txt", operation_id.id.as_str()));
    let transcript_path = artifact_dir.join("transcript.log");
    if let Some(parent) = raw_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let output = format!(
        "mock debug worker accepted eval command; real DbgEng execution is not wired yet\ncommand: {}\n",
        command
    );
    fs::write(&raw_path, &output)?;
    let transcript = format!("> {}\n{}\n", command, output);
    let mut transcript_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript_path)?;
    transcript_file.write_all(transcript.as_bytes())?;

    Ok(WorkerResponse::DebugCommand {
        result: DebugCommandResult {
            session_id,
            operation_id: None,
            command,
            output: output.clone(),
            final_state: Some(DebugSessionState::Ready),
            raw_output: None,
            warnings: vec!["mock worker: real DbgEng execution is not wired yet".to_string()],
            error: None,
        },
        writes: vec![
            WorkerArtifactWrite {
                relative_path: raw_relative_path,
                kind: "debug.raw_output".to_string(),
                byte_len: output.len() as u64,
                description: Some("mock debug eval raw output".to_string()),
            },
            WorkerArtifactWrite {
                relative_path: transcript_relative_path,
                kind: "debug.transcript".to_string(),
                byte_len: transcript.len() as u64,
                description: Some("debug session transcript".to_string()),
            },
        ],
    })
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
