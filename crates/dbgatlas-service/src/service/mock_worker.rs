use super::*;

pub(super) fn mock_worker_response(request: WorkerRequest) -> Result<WorkerResponse, ServiceError> {
    match request {
        WorkerRequest::Recording {
            session_id,
            operation_id,
            action,
            ..
        } => {
            let (state, status) = match action.as_str() {
                "recording.start" => ("running", "success"),
                "recording.stop" => ("stopped", "success"),
                "recording.cancel" => ("canceled", "canceled"),
                _ => {
                    return Err(ServiceError::Worker(
                        "mock recording action unsupported".into(),
                    ));
                }
            };
            Ok(WorkerResponse::Recording {
                result: json!({"recording_id":session_id,"session_id":session_id,
                "operation_id":operation_id,"state":state,"operation_status":status,"artifact_refs":[],
                "warnings":["mock worker; no collection performed"]}),
                writes: vec![],
            })
        }
        WorkerRequest::StartDebugSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session started by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::EvalDebugCommand {
            session_id,
            operation_id,
            command,
            artifact_dir,
        } => write_mock_eval_response(session_id, operation_id, command, artifact_dir),
        WorkerRequest::AddSymbols {
            session_id,
            operation_id,
            symbol_path,
            reload,
            artifact_dir,
        } => {
            let command = if reload {
                format!(".sympath+ {symbol_path}; .reload")
            } else {
                format!(".sympath+ {symbol_path}")
            };
            write_mock_eval_response(session_id, operation_id, command, artifact_dir)
        }
        WorkerRequest::ReadMemory {
            session_id,
            operation_id,
            address,
            length,
            artifact_dir,
        } => write_mock_memory_response(session_id, operation_id, address, length, artifact_dir),
        WorkerRequest::CloseSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session closed by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::KillSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session killed by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::CancelOperation { .. } => Ok(WorkerResponse::Ok {
            summary: "operation canceled by mock worker".to_string(),
            writes: Vec::new(),
        }),
    }
}

pub(super) fn write_mock_eval_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    command: String,
    artifact_dir: PathBuf,
) -> Result<WorkerResponse, ServiceError> {
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
    let output = if command == ".mock_long_output" {
        "x".repeat(DEFAULT_INLINE_RESULT_BYTE_LIMIT + 100)
    } else {
        format!(
            "mock debug worker accepted eval command; real DbgEng execution is not wired yet\ncommand: {}\n",
            command
        )
    };
    write_atomic_bytes(&raw_path, output.as_bytes())?;
    let transcript = format!("> {}\n{}\n", command, output);
    let mut transcript_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript_path)?;
    transcript_file.write_all(transcript.as_bytes())?;
    let mut events_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)?;
    serde_json::to_writer(
        &mut events_file,
        &json!({
            "event": "output",
            "session_id": session_id,
            "operation_id": operation_id,
            "command": command,
            "timestamp": Timestamp::now(),
            "byte_len": output.len(),
        }),
    )?;
    events_file.write_all(b"\n")?;
    let preview = ResultPreview::utf8(&output, DEFAULT_INLINE_RESULT_BYTE_LIMIT);

    Ok(WorkerResponse::DebugCommand {
        result: DebugCommandResult {
            session_id,
            operation_id: None,
            command,
            result: preview,
            final_state: Some(DebugSessionState::Ready),
            warnings: vec!["mock worker: real DbgEng execution is not wired yet".to_string()],
            error: None,
        },
        writes: vec![
            WorkerArtifactWrite {
                relative_path: raw_relative_path,
                kind: "debug.raw_output".to_string(),
                role: Some(WorkerArtifactRole::FullResult),
                byte_len: output.len() as u64,
                description: Some("mock debug eval raw output".to_string()),
            },
            WorkerArtifactWrite {
                relative_path: transcript_relative_path,
                kind: "debug.transcript".to_string(),
                role: None,
                byte_len: transcript.len() as u64,
                description: Some("debug session transcript".to_string()),
            },
            WorkerArtifactWrite {
                relative_path: events_relative_path,
                kind: "debug.events".to_string(),
                role: None,
                byte_len: output.len() as u64,
                description: Some("debug session events".to_string()),
            },
        ],
    })
}

pub(super) fn write_mock_memory_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    address: u64,
    length: u64,
    artifact_dir: PathBuf,
) -> Result<WorkerResponse, ServiceError> {
    let length = usize::try_from(length)
        .map_err(|_| ServiceError::Worker("memory read length is too large".to_string()))?;
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
    let bytes = vec![0u8; length];
    fs::write(&memory_path, &bytes)?;

    Ok(WorkerResponse::DebugMemory {
        result: DebugMemoryResult {
            session_id,
            operation_id: None,
            address,
            requested_length: length as u64,
            bytes_read: length as u64,
            memory: None,
            warnings: vec!["mock worker: real DbgEng memory read is not wired yet".to_string()],
            error: None,
        },
        writes: vec![WorkerArtifactWrite {
            relative_path,
            kind: "debug.memory".to_string(),
            role: None,
            byte_len: length as u64,
            description: Some("mock debug memory read".to_string()),
        }],
    })
}
