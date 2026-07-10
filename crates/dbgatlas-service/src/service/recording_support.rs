use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn write_recording_start_artifacts(
    artifact_dir: &Path,
    recording_id: &RecordingRef,
    operation_id: &OperationRef,
    request: &StartRecording,
    root_pid: Option<u32>,
    now: Timestamp,
    trace_start_error: Option<&str>,
    trace_consumer_error: Option<&str>,
    trace_stack_status: Option<dbgatlas_etw::EtwStackTraceStatus>,
    trace_stack_status_error: Option<&str>,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    fs::create_dir_all(artifact_dir.join("events"))?;
    let metadata = recording_metadata_json(
        recording_id,
        operation_id,
        &request.target,
        &request.presets,
        root_pid,
        "running",
        now,
        None,
        etw_adapter_metadata(),
        trace_start_error,
        trace_consumer_error,
        trace_stack_status,
        trace_stack_status_error,
        None,
    );
    let metadata_snapshot = format!("metadata/{}.json", operation_id.id.as_str());
    let metadata_len = write_json_file(&artifact_dir.join(&metadata_snapshot), &metadata)?;
    let _ = write_json_file(&artifact_dir.join("recording.json"), &metadata)?;
    let process_event = normalized_recording_event(
        recording_id,
        operation_id,
        "process",
        "recording_started",
        root_pid,
        now,
    );
    let process_len = write_jsonl_file(
        &artifact_dir.join("events").join("process.jsonl"),
        &process_event,
    )?;

    Ok(vec![
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, &metadata_snapshot),
            kind: "recording.metadata".to_string(),
            role: None,
            byte_len: metadata_len,
            description: Some("recording start metadata snapshot".to_string()),
        },
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "events/process.jsonl"),
            kind: "recording.events.process".to_string(),
            role: None,
            byte_len: process_len,
            description: Some("recording process events".to_string()),
        },
    ])
}

pub(super) fn write_recording_stop_artifacts(
    artifact_dir: &Path,
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    now: Timestamp,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    fs::create_dir_all(artifact_dir.join("events"))?;
    let mut writes = write_recording_terminal_trace_artifacts(
        artifact_dir,
        recording,
        operation_id,
        now,
        "stopped",
    )?;

    for preset in &recording.presets {
        let suffix = format!("events/{}", preset.artifact_file_name());
        let path = artifact_dir.join(&suffix);
        if path.is_file() && fs::metadata(&path)?.len() > 0 {
            continue;
        }
        let event = normalized_recording_event(
            &recording.recording_id,
            operation_id,
            preset.category(),
            "recording_stopped",
            recording.root_pid,
            now,
        );
        let byte_len = write_jsonl_file(&path, &event)?;
        writes.push(WorkerArtifactWrite {
            relative_path: recording_relative_path(&recording.recording_id, &suffix),
            kind: format!("recording.events.{}", preset.category()),
            role: None,
            byte_len,
            description: Some(format!("recording {} events", preset.category())),
        });
    }

    Ok(writes)
}

pub(super) struct TraceArtifactOutcome {
    pub(super) byte_len: u64,
    pub(super) description: String,
    pub(super) valid_etl: bool,
    pub(super) fallback_reason: Option<String>,
    pub(super) filter: EtwProcessingOutcome,
    pub(super) extraction: EtwProcessingOutcome,
}

#[derive(Clone, Debug, Default)]
pub(super) struct EtwProcessingOutcome {
    pub(super) description: String,
    pub(super) result: Option<dbgatlas_etw::EtwEventExtractionResult>,
    pub(super) error: Option<String>,
}

pub(super) fn write_recording_terminal_trace_artifacts(
    artifact_dir: &Path,
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    now: Timestamp,
    state: &str,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    let trace_path = artifact_dir.join("trace.etl");
    let trace_outcome = write_recording_trace_artifact(recording, &trace_path)?;
    let mut writes = write_recording_terminal_metadata(
        artifact_dir,
        recording,
        operation_id,
        now,
        state,
        Some(&trace_outcome),
    )?;
    writes.push(WorkerArtifactWrite {
        relative_path: recording_relative_path(&recording.recording_id, "trace.etl"),
        kind: "recording.trace".to_string(),
        role: None,
        byte_len: trace_outcome.byte_len,
        description: Some(trace_outcome.description),
    });
    register_recording_event_writes(artifact_dir, recording, &mut writes)?;
    Ok(writes)
}

pub(super) fn write_recording_trace_artifact(
    recording: &ManagedRecording,
    trace_path: &Path,
) -> Result<TraceArtifactOutcome, ServiceError> {
    let mut session = recording
        .trace_session
        .lock()
        .map_err(|_| ServiceError::Worker("recording trace session lock poisoned".to_string()))?;
    if let Some(session) = session.take() {
        match session.stop() {
            Ok(()) => {
                let filter_note = filter_recording_trace(recording, trace_path);
                let extraction_note = extract_recording_events(recording, trace_path);
                let byte_len = fs::metadata(trace_path)?.len();
                let description = format!(
                    "ETW file trace; {}; {}",
                    filter_note.description, extraction_note.description
                );
                return Ok(TraceArtifactOutcome {
                    byte_len,
                    description,
                    valid_etl: true,
                    fallback_reason: None,
                    filter: filter_note,
                    extraction: extraction_note,
                });
            }
            Err(error) => {
                let trace_text =
                    format!("DbgAtlas fallback trace artifact.\nNative ETW stop failed: {error}\n");
                fs::write(trace_path, trace_text.as_bytes())?;
                return Ok(TraceArtifactOutcome {
                    byte_len: trace_text.len() as u64,
                    description: "fallback trace artifact with native ETW stop error".to_string(),
                    valid_etl: false,
                    fallback_reason: Some(format!("Native ETW stop failed: {error}")),
                    filter: EtwProcessingOutcome::default(),
                    extraction: EtwProcessingOutcome::default(),
                });
            }
        }
    }

    let reason = recording
        .trace_start_error
        .as_deref()
        .unwrap_or("native ETW trace session was not started");
    let trace_text =
        format!("DbgAtlas fallback trace artifact.\nNative ETW start failed: {reason}\n");
    fs::write(trace_path, trace_text.as_bytes())?;
    Ok(TraceArtifactOutcome {
        byte_len: trace_text.len() as u64,
        description: "fallback trace artifact with native ETW start error".to_string(),
        valid_etl: false,
        fallback_reason: Some(format!("Native ETW start failed: {reason}")),
        filter: EtwProcessingOutcome::default(),
        extraction: EtwProcessingOutcome::default(),
    })
}

pub(super) fn register_recording_event_writes(
    artifact_dir: &Path,
    recording: &ManagedRecording,
    writes: &mut Vec<WorkerArtifactWrite>,
) -> Result<(), ServiceError> {
    for preset in &recording.presets {
        let suffix = format!("events/{}", preset.artifact_file_name());
        let path = artifact_dir.join(&suffix);
        let size_after = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if size_after == 0 {
            continue;
        }
        writes.push(WorkerArtifactWrite {
            relative_path: recording_relative_path(&recording.recording_id, &suffix),
            kind: format!("recording.events.{}", preset.category()),
            role: None,
            byte_len: size_after,
            description: Some(format!("recording {} events", preset.category())),
        });
    }
    Ok(())
}

pub(super) fn filter_recording_trace(
    recording: &ManagedRecording,
    trace_path: &Path,
) -> EtwProcessingOutcome {
    let filtered_path = recording.artifact_dir.join("trace.filtered.etl");
    match dbgatlas_etw::filter_trace_file(
        trace_path,
        &filtered_path,
        etw_preset_flags(&recording.presets),
        recording.root_pid,
    ) {
        Ok(result) => match fs::copy(&filtered_path, trace_path) {
            Ok(_) => {
                let _ = fs::remove_file(&filtered_path);
                EtwProcessingOutcome {
                    description: format!(
                        "filtered ETL wrote {} events across {} categories and skipped {}",
                        result.events_written, result.files_written, result.skipped_events
                    ),
                    result: Some(result),
                    error: None,
                }
            }
            Err(error) => {
                let _ = fs::remove_file(&filtered_path);
                EtwProcessingOutcome {
                    description: format!("filtered ETL copy failed: {error}"),
                    result: Some(result),
                    error: Some(error.to_string()),
                }
            }
        },
        Err(error) => {
            let _ = fs::remove_file(&filtered_path);
            EtwProcessingOutcome {
                description: format!("filtered ETL fallback to original trace: {error}"),
                result: None,
                error: Some(error.to_string()),
            }
        }
    }
}

pub(super) fn extract_recording_events(
    recording: &ManagedRecording,
    trace_path: &Path,
) -> EtwProcessingOutcome {
    let events_dir = recording.artifact_dir.join("events");
    match dbgatlas_etw::extract_file_events(
        trace_path,
        &events_dir,
        etw_preset_flags(&recording.presets),
        recording.root_pid,
    ) {
        Ok(result) => EtwProcessingOutcome {
            description: format!(
                "event extraction wrote {} events to {} files and skipped {}",
                result.events_written, result.files_written, result.skipped_events
            ),
            result: Some(result),
            error: None,
        },
        Err(error) => EtwProcessingOutcome {
            description: format!("event extraction skipped: {error}"),
            result: None,
            error: Some(error.to_string()),
        },
    }
}

pub(super) fn write_recording_terminal_metadata(
    artifact_dir: &Path,
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    now: Timestamp,
    state: &str,
    trace_outcome: Option<&TraceArtifactOutcome>,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    let metadata = recording_metadata_json(
        &recording.recording_id,
        operation_id,
        &recording.target,
        &recording.presets,
        recording.root_pid,
        state,
        recording.created_at,
        Some(now),
        etw_adapter_metadata(),
        recording.trace_start_error.as_deref(),
        recording.trace_consumer_error.as_deref(),
        recording.trace_stack_status,
        recording.trace_stack_status_error.as_deref(),
        trace_outcome,
    );
    let metadata_len = write_json_file(&artifact_dir.join("recording.json"), &metadata)?;
    Ok(vec![WorkerArtifactWrite {
        relative_path: recording_relative_path(&recording.recording_id, "recording.json"),
        kind: "recording.metadata".to_string(),
        role: None,
        byte_len: metadata_len,
        description: Some("recording metadata".to_string()),
    }])
}

#[allow(clippy::too_many_arguments)]
pub(super) fn recording_metadata_json(
    recording_id: &RecordingRef,
    operation_id: &OperationRef,
    target: &RecordingTarget,
    presets: &[RecordingPreset],
    root_pid: Option<u32>,
    state: &str,
    started_at: Timestamp,
    stopped_at: Option<Timestamp>,
    adapter: EtwAdapterMetadata,
    trace_start_error: Option<&str>,
    trace_consumer_error: Option<&str>,
    trace_stack_status: Option<dbgatlas_etw::EtwStackTraceStatus>,
    trace_stack_status_error: Option<&str>,
    trace_outcome: Option<&TraceArtifactOutcome>,
) -> Value {
    json!({
        "recording_id": recording_id,
        "target": target,
        "mode": target.mode(),
        "root_pid": root_pid,
        "process_tree_filter": {
            "root_pid": root_pid,
            "mode": "process_tree"
        },
        "presets": presets,
        "etw_preset_flags": etw_preset_flags(presets).bits(),
        "state": state,
        "started_at": started_at,
        "stopped_at": stopped_at,
        "adapter": adapter,
        "stack_trace": stack_trace_metadata(
            trace_stack_status,
            trace_stack_status_error,
            trace_start_error,
        ),
        "trace_start_error": trace_start_error,
        "trace_consumer_error": trace_consumer_error,
        "trace": trace_metadata(trace_outcome),
        "event_extraction": event_extraction_metadata(trace_outcome),
        "operation_id": operation_id,
    })
}

pub(super) fn trace_metadata(outcome: Option<&TraceArtifactOutcome>) -> Value {
    match outcome {
        Some(outcome) => json!({
            "valid_etl": outcome.valid_etl,
            "fallback_reason": outcome.fallback_reason,
            "filter": processing_metadata(&outcome.filter),
        }),
        None => json!({
            "valid_etl": null,
            "fallback_reason": null,
            "filter": null,
        }),
    }
}

pub(super) fn event_extraction_metadata(outcome: Option<&TraceArtifactOutcome>) -> Value {
    match outcome {
        Some(outcome) => processing_metadata(&outcome.extraction),
        None => json!(null),
    }
}

pub(super) fn processing_metadata(outcome: &EtwProcessingOutcome) -> Value {
    let warnings = outcome.result.map(extraction_warnings).unwrap_or_default();
    json!({
        "description": outcome.description,
        "error": outcome.error,
        "result": outcome.result,
        "warnings": warnings,
    })
}

pub(super) fn extraction_warnings(result: dbgatlas_etw::EtwEventExtractionResult) -> Vec<String> {
    let mut warnings = Vec::new();
    if result.unmatched_op_end > 0 {
        warnings.push(format!(
            "native ETW file_io saw {} OpEnd events without a matching begin event",
            result.unmatched_op_end
        ));
    }
    if result.reused_irp > 0 {
        warnings.push(format!(
            "native ETW file_io saw {} reused IrpPtr values before a matching OpEnd",
            result.reused_irp
        ));
    }
    if result.incomplete_io > 0 {
        warnings.push(format!(
            "native ETW file_io left {} begin events without a matching OpEnd",
            result.incomplete_io
        ));
    }
    if result.file_path_unresolved > 0 {
        warnings.push(format!(
            "native ETW file_io left {} target file events without a resolved path",
            result.file_path_unresolved
        ));
    }
    if result.dropped_stack_walk > 0 {
        warnings.push(format!(
            "native ETW dropped {} unmatched StackWalk events while bounding the pending stack cache",
            result.dropped_stack_walk
        ));
    }
    warnings
}

pub(super) fn stack_trace_metadata(
    status: Option<dbgatlas_etw::EtwStackTraceStatus>,
    status_error: Option<&str>,
    trace_start_error: Option<&str>,
) -> Value {
    let mut warnings = Vec::new();
    if let Some(error) = trace_start_error {
        warnings.push(format!("native ETW trace session was not started: {error}"));
    }
    if let Some(error) = status_error {
        warnings.push(format!("stack trace status unavailable: {error}"));
    }
    if let Some(status) = status {
        if status.provider_stack_warning_count > 0 {
            warnings.push(format!(
                "stack trace provider enable fell back without stack for {} provider(s)",
                status.provider_stack_warning_count
            ));
        }
        if status.kernel_stack_warning_count > 0 {
            warnings.push(format!(
                "kernel stack tracing enable failed for {} configuration attempt(s)",
                status.kernel_stack_warning_count
            ));
        }
        json!({
            "requested": status.requested,
            "enabled": status.enabled,
            "provider_stack_enabled": status.provider_stack_enabled,
            "kernel_stack_enabled": status.kernel_stack_enabled,
            "warnings": warnings,
        })
    } else {
        json!({
            "requested": true,
            "enabled": false,
            "provider_stack_enabled": false,
            "kernel_stack_enabled": false,
            "warnings": warnings,
        })
    }
}

pub(super) fn etw_preset_flags(presets: &[RecordingPreset]) -> dbgatlas_etw::EtwPresetFlags {
    let mut flags = dbgatlas_etw::EtwPresetFlags::empty();
    for preset in presets {
        flags.insert(match preset {
            RecordingPreset::Process => dbgatlas_etw::EtwPresetFlags::PROCESS,
            RecordingPreset::Thread => dbgatlas_etw::EtwPresetFlags::THREAD,
            RecordingPreset::Image => dbgatlas_etw::EtwPresetFlags::IMAGE,
            RecordingPreset::File => dbgatlas_etw::EtwPresetFlags::FILE,
            RecordingPreset::Registry => dbgatlas_etw::EtwPresetFlags::REGISTRY,
            RecordingPreset::Network => dbgatlas_etw::EtwPresetFlags::NETWORK,
        });
    }
    flags
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct EtwAdapterMetadata {
    pub(super) id: &'static str,
    pub(super) native: &'static str,
    pub(super) version: Option<dbgatlas_etw::NativeVersion>,
    pub(super) capabilities: Option<dbgatlas_etw::EtwCapabilities>,
    pub(super) note: String,
}

pub(super) fn etw_adapter_metadata() -> EtwAdapterMetadata {
    match dbgatlas_etw::adapter_info() {
        Ok(info) => EtwAdapterMetadata {
            id: "etw",
            native: "available",
            version: Some(info.version),
            capabilities: Some(info.capabilities),
            note:
                "native ETW adapter ABI is available; file trace and ETL event extraction are wired"
                    .to_string(),
        },
        Err(error) => EtwAdapterMetadata {
            id: "etw",
            native: "unavailable",
            version: None,
            capabilities: None,
            note: error.to_string(),
        },
    }
}

pub(super) fn normalized_recording_event(
    recording_id: &RecordingRef,
    operation_id: &OperationRef,
    category: &str,
    event_type: &str,
    pid: Option<u32>,
    timestamp: Timestamp,
) -> Value {
    json!({
        "schema_version": 1,
        "recording_id": recording_id,
        "timestamp": timestamp,
        "category": category,
        "event_type": event_type,
        "pid": pid,
        "tid": null,
        "process": {
            "pid": pid,
            "parent_pid": null,
            "image_path": null,
            "command_line": null
        },
        "operation_id": operation_id,
        "artifact_id": null,
        "etw": {
            "provider": "dbg_atlas_placeholder",
            "event_id": 0,
            "version": 1,
            "opcode": event_type,
            "keywords": [category],
            "raw": {}
        }
    })
}

pub(super) enum PreparedRecordingTarget {
    Attach { pid: u32 },
    Launch(PreparedLaunch),
}

impl PreparedRecordingTarget {
    pub(super) fn root_pid(&self) -> u32 {
        match self {
            Self::Attach { pid } => *pid,
            Self::Launch(launch) => launch.pid(),
        }
    }

    pub(super) fn resume(self) -> Result<(), ServiceError> {
        match self {
            Self::Attach { .. } => Ok(()),
            Self::Launch(launch) => launch.resume(),
        }
    }
}

pub(super) fn prepare_recording_target(
    target: &RecordingTarget,
) -> Result<PreparedRecordingTarget, ServiceError> {
    match target {
        RecordingTarget::Launch { executable, args } => Ok(PreparedRecordingTarget::Launch(
            PreparedLaunch::create(executable, args)?,
        )),
        RecordingTarget::Attach { pid } => Ok(PreparedRecordingTarget::Attach { pid: *pid }),
    }
}

pub(super) enum TtdRecorderProcess {
    Std(Child),
    #[cfg(windows)]
    RawWindows(windows_active_user_process::RawProcess),
}

impl TtdRecorderProcess {
    pub(super) fn try_wait(&mut self) -> Result<Option<Option<i32>>, std::io::Error> {
        match self {
            Self::Std(child) => child
                .try_wait()
                .map(|status| status.map(|status| status.code())),
            #[cfg(windows)]
            Self::RawWindows(process) => process.try_wait(),
        }
    }

    pub(super) fn wait(&mut self) -> Result<Option<i32>, std::io::Error> {
        match self {
            Self::Std(child) => child.wait().map(|status| status.code()),
            #[cfg(windows)]
            Self::RawWindows(process) => process.wait_code(),
        }
    }

    pub(super) fn kill(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::Std(child) => child.kill(),
            #[cfg(windows)]
            Self::RawWindows(process) => process.kill(),
        }
    }
}

pub(super) fn spawn_ttd_process(
    ttd_exe: &Path,
    args: &[OsString],
    current_dir: Option<&Path>,
    worker_identity: TtdRecorderIdentity,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<TtdRecorderProcess, ServiceError> {
    if let Some(parent) = stdout_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = stderr_path.parent() {
        fs::create_dir_all(parent)?;
    }
    match worker_identity {
        TtdRecorderIdentity::Default => {
            let stdout = fs::File::create(stdout_path)?;
            let stderr = fs::File::create(stderr_path)?;
            let mut command = Command::new(ttd_exe);
            command
                .args(args)
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            if let Some(current_dir) = current_dir {
                command.current_dir(current_dir);
            }
            Ok(TtdRecorderProcess::Std(command.spawn()?))
        }
        TtdRecorderIdentity::ActiveInteractiveUser => spawn_active_interactive_ttd_process(
            ttd_exe,
            args,
            current_dir,
            stdout_path,
            stderr_path,
        ),
    }
}

#[cfg(windows)]
pub(super) fn spawn_active_interactive_ttd_process(
    ttd_exe: &Path,
    args: &[OsString],
    current_dir: Option<&Path>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<TtdRecorderProcess, ServiceError> {
    let helper_exe = std::env::current_exe()?;
    let helper_args = ttd_command_helper_args(ttd_exe, args, current_dir, stdout_path, stderr_path);
    windows_active_user_process::spawn_os(&helper_exe, &helper_args)
        .map(TtdRecorderProcess::RawWindows)
}

pub(super) fn ttd_command_helper_args(
    ttd_exe: &Path,
    args: &[OsString],
    current_dir: Option<&Path>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Vec<OsString> {
    let mut helper_args = vec![
        OsString::from("service"),
        OsString::from("run-ttd-command"),
        OsString::from("--executable"),
        ttd_exe.as_os_str().to_os_string(),
        OsString::from("--stdout-path"),
        stdout_path.as_os_str().to_os_string(),
        OsString::from("--stderr-path"),
        stderr_path.as_os_str().to_os_string(),
    ];
    if let Some(current_dir) = current_dir {
        helper_args.push(OsString::from("--current-dir"));
        helper_args.push(current_dir.as_os_str().to_os_string());
    }
    helper_args.push(OsString::from("--"));
    helper_args.extend(args.iter().cloned());
    helper_args
}

#[cfg(not(windows))]
pub(super) fn spawn_active_interactive_ttd_process(
    _ttd_exe: &Path,
    _args: &[OsString],
    _current_dir: Option<&Path>,
    _stdout_path: &Path,
    _stderr_path: &Path,
) -> Result<TtdRecorderProcess, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}

pub(super) fn run_ttd_process(
    invocation: TtdRecorderInvocation,
) -> Result<TtdProcessExit, ServiceError> {
    let mut child = spawn_ttd_process(
        &invocation.ttd_exe,
        &invocation.args,
        invocation.current_dir.as_deref(),
        invocation.worker_identity,
        &invocation.stdout_path,
        &invocation.stderr_path,
    )?;
    let deadline = Instant::now() + invocation.timeout;
    loop {
        if let Some(exit_code) = child.try_wait()? {
            return Ok(TtdProcessExit {
                exit_code,
                timed_out: false,
                stop: None,
                killed_after_timeout: false,
            });
        }
        if Instant::now() >= deadline {
            let mut stop = None;
            if let Some(timeout_stop) = invocation.timeout_stop.as_ref() {
                let recorder_exit_deadline = Instant::now() + timeout_stop.recorder_exit_timeout;
                let mut timeout_stop = timeout_stop.clone();
                if timeout_stop.prefer_recorded_pid {
                    let recorder_output = read_text_lossy(&invocation.stdout_path)
                        + "\n"
                        + read_text_lossy(&invocation.stderr_path).as_str();
                    if let Some(pid) = parse_first_recorded_pid(&recorder_output) {
                        timeout_stop.stop_target = OsString::from(pid.to_string());
                    }
                }
                stop = Some(run_ttd_stop_process(
                    &invocation.ttd_exe,
                    &timeout_stop,
                    invocation.worker_identity,
                ));
                while Instant::now() < recorder_exit_deadline {
                    if let Some(exit_code) = child.try_wait()? {
                        return Ok(TtdProcessExit {
                            exit_code,
                            timed_out: true,
                            stop,
                            killed_after_timeout: false,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            let (exit_code, killed_after_timeout) = if let Some(exit_code) = child.try_wait()? {
                (exit_code, false)
            } else {
                let _ = child.kill();
                (child.wait().ok().flatten(), true)
            };
            return Ok(TtdProcessExit {
                exit_code,
                timed_out: true,
                stop,
                killed_after_timeout,
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn run_ttd_stop_process(
    ttd_exe: &Path,
    timeout_stop: &TtdTimeoutStop,
    worker_identity: TtdRecorderIdentity,
) -> TtdStopExit {
    let args = ttd_stop_args(&timeout_stop.stop_target);
    match run_ttd_command(
        ttd_exe,
        &args,
        worker_identity,
        timeout_stop.timeout,
        &timeout_stop.stdout_path,
        &timeout_stop.stderr_path,
    ) {
        Ok(exit) => TtdStopExit {
            stop_target: timeout_stop.stop_target.clone(),
            exit_code: exit.exit_code,
            timed_out: exit.timed_out,
            error: None,
        },
        Err(error) => {
            let _ = ensure_file_exists(&timeout_stop.stdout_path);
            let _ = ensure_file_exists(&timeout_stop.stderr_path);
            TtdStopExit {
                stop_target: timeout_stop.stop_target.clone(),
                exit_code: None,
                timed_out: false,
                error: Some(error.to_string()),
            }
        }
    }
}

pub(super) fn ttd_stop_args(stop_target: &OsStr) -> Vec<OsString> {
    vec![OsString::from("-stop"), stop_target.to_os_string()]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TtdCommandExit {
    pub(super) exit_code: Option<i32>,
    pub(super) timed_out: bool,
}

pub(super) fn run_ttd_command(
    ttd_exe: &Path,
    args: &[OsString],
    worker_identity: TtdRecorderIdentity,
    timeout: Duration,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<TtdCommandExit, ServiceError> {
    let mut child = spawn_ttd_process(
        ttd_exe,
        args,
        None,
        worker_identity,
        stdout_path,
        stderr_path,
    )?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(exit_code) = child.try_wait()? {
            return Ok(TtdCommandExit {
                exit_code,
                timed_out: false,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let exit_code = child.wait().ok().flatten();
            return Ok(TtdCommandExit {
                exit_code,
                timed_out: true,
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct DiscoveredTtdArtifacts {
    pub(super) traces: Vec<PathBuf>,
    pub(super) trace_indexes: Vec<PathBuf>,
    pub(super) recorder_logs: Vec<PathBuf>,
}

pub(super) struct TtdRecordingMetadata<'a> {
    pub(super) recording_id: &'a RecordingRef,
    pub(super) operation_id: &'a OperationRef,
    pub(super) request: &'a RecordTtd,
    pub(super) status: &'a str,
    pub(super) operation_status: &'a OperationStatus,
    pub(super) target_pid: Option<u32>,
    pub(super) recorder_exit_code: Option<i32>,
    pub(super) timed_out: bool,
    pub(super) started_at: Timestamp,
    pub(super) stopped_at: Timestamp,
    pub(super) duration_ms: u64,
    pub(super) ttd_exe: &'a Path,
    pub(super) worker_identity: TtdRecorderIdentity,
    pub(super) discovered: &'a DiscoveredTtdArtifacts,
    pub(super) warnings: &'a [String],
    pub(super) error: Option<&'a str>,
}

pub(super) fn append_ttd_recording_event(
    events_path: &Path,
    recording_id: &RecordingRef,
    operation_id: &OperationRef,
    event: &str,
    timestamp: Timestamp,
    fields: Value,
    error: Option<String>,
) -> Result<u64, ServiceError> {
    write_jsonl_file(
        events_path,
        &json!({
            "event": event,
            "recording_id": recording_id,
            "operation_id": operation_id,
            "timestamp": timestamp,
            "fields": fields,
            "error": error,
        }),
    )
}

pub(super) fn ttd_launch_start_directory(
    target: &TtdTarget,
) -> Result<Option<PathBuf>, ServiceError> {
    let Some(start_directory) = (match target {
        TtdTarget::Launch {
            start_directory, ..
        } => start_directory.as_ref(),
        _ => None,
    }) else {
        return Ok(None);
    };
    if !start_directory.is_dir() {
        return Err(ServiceError::Rpc(format!(
            "TTD launch start_directory does not exist or is not a directory: {}",
            start_directory.display()
        )));
    }
    Ok(Some(start_directory.clone()))
}

pub(super) fn discover_ttd_artifacts(
    traces_dir: &Path,
) -> Result<DiscoveredTtdArtifacts, ServiceError> {
    let mut discovered = DiscoveredTtdArtifacts::default();
    if !traces_dir.exists() {
        return Ok(discovered);
    }
    for entry in fs::read_dir(traces_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "run" => discovered.traces.push(path),
            "idx" => discovered.trace_indexes.push(path),
            "out" | "err" | "log" => discovered.recorder_logs.push(path),
            _ => {}
        }
    }
    discovered.traces.sort();
    discovered.trace_indexes.sort();
    discovered.recorder_logs.sort();
    Ok(discovered)
}

pub(super) fn ttd_recording_metadata_json(input: TtdRecordingMetadata<'_>) -> Value {
    json!({
        "recording_id": input.recording_id,
        "operation_id": input.operation_id,
        "target": input.request.target,
        "mode": input.request.target.mode(),
        "worker_identity": input.worker_identity,
        "timeout_ms": input.request.timeout_ms,
        "options": input.request.options,
        "target_pid": input.target_pid,
        "started_at": input.started_at,
        "stopped_at": input.stopped_at,
        "duration_ms": input.duration_ms,
        "status": input.status,
        "operation_status": input.operation_status,
        "recorder_exit_code": input.recorder_exit_code,
        "timed_out": input.timed_out,
        "adapter": {
            "kind": "ttd",
            "tool": "TTD.exe",
            "ttd_exe": input.ttd_exe,
            "worker_identity": input.worker_identity,
        },
        "traces": input.discovered.traces,
        "trace_indexes": input.discovered.trace_indexes,
        "recorder_logs": input.discovered.recorder_logs,
        "warnings": input.warnings,
        "error": input.error,
    })
}

pub(super) fn ttd_recording_writes(
    recording_id: &RecordingRef,
    artifact_dir: &Path,
    metadata_len: u64,
    include_stop_stdout: bool,
    include_stop_stderr: bool,
    discovered: &DiscoveredTtdArtifacts,
) -> Result<Vec<WorkerArtifactWrite>, ServiceError> {
    let mut writes = vec![
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recording.json"),
            kind: "recording.metadata".to_string(),
            role: None,
            byte_len: metadata_len,
            description: Some("TTD recording metadata".to_string()),
        },
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "events.jsonl"),
            kind: "recording.events.ttd".to_string(),
            role: None,
            byte_len: file_len_or_zero(&artifact_dir.join("events.jsonl"))?,
            description: Some("TTD recording events".to_string()),
        },
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recorder.stdout.txt"),
            kind: "recording.recorder_output".to_string(),
            role: None,
            byte_len: file_len_or_zero(&artifact_dir.join("recorder.stdout.txt"))?,
            description: Some("TTD recorder stdout".to_string()),
        },
        WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recorder.stderr.txt"),
            kind: "recording.recorder_output".to_string(),
            role: None,
            byte_len: file_len_or_zero(&artifact_dir.join("recorder.stderr.txt"))?,
            description: Some("TTD recorder stderr".to_string()),
        },
    ];
    if include_stop_stdout {
        writes.push(WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recorder-stop.stdout.txt"),
            kind: "recording.recorder_output".to_string(),
            role: None,
            byte_len: file_len_or_zero(&artifact_dir.join("recorder-stop.stdout.txt"))?,
            description: Some("TTD recorder stop stdout".to_string()),
        });
    }
    if include_stop_stderr {
        writes.push(WorkerArtifactWrite {
            relative_path: recording_relative_path(recording_id, "recorder-stop.stderr.txt"),
            kind: "recording.recorder_output".to_string(),
            role: None,
            byte_len: file_len_or_zero(&artifact_dir.join("recorder-stop.stderr.txt"))?,
            description: Some("TTD recorder stop stderr".to_string()),
        });
    }
    for trace in &discovered.traces {
        writes.push(discovered_ttd_write(
            recording_id,
            artifact_dir,
            trace,
            "recording.ttd.trace",
            "TTD .run trace",
        )?);
    }
    for index in &discovered.trace_indexes {
        writes.push(discovered_ttd_write(
            recording_id,
            artifact_dir,
            index,
            "recording.ttd.index",
            "TTD trace index",
        )?);
    }
    for log in &discovered.recorder_logs {
        writes.push(discovered_ttd_write(
            recording_id,
            artifact_dir,
            log,
            "recording.recorder_output",
            "TTD recorder log",
        )?);
    }
    Ok(writes)
}

pub(super) fn ttd_trace_artifact_results(
    workspace: &Workspace,
    writes: &[WorkerArtifactWrite],
    registered: &RegisteredWorkerWrites,
) -> Vec<Value> {
    writes
        .iter()
        .zip(registered.artifacts.iter())
        .filter(|(write, _)| write.kind == "recording.ttd.trace")
        .map(|(write, artifact_ref)| {
            json!({
                "path": workspace.root().join(&write.relative_path),
                "relative_path": &write.relative_path,
                "artifact_ref": artifact_ref,
                "byte_len": write.byte_len,
            })
        })
        .collect()
}

pub(super) fn discovered_ttd_write(
    recording_id: &RecordingRef,
    artifact_dir: &Path,
    path: &Path,
    kind: &str,
    description: &str,
) -> Result<WorkerArtifactWrite, ServiceError> {
    let relative_to_recording = path.strip_prefix(artifact_dir).map_err(|_| {
        ServiceError::Rpc(format!(
            "TTD artifact escaped recording dir: {}",
            path.display()
        ))
    })?;
    Ok(WorkerArtifactWrite {
        relative_path: recording_relative_path(
            recording_id,
            &relative_to_recording.to_string_lossy(),
        ),
        kind: kind.to_string(),
        role: None,
        byte_len: fs::metadata(path)?.len(),
        description: Some(description.to_string()),
    })
}

pub(super) fn file_len_or_zero(path: &Path) -> Result<u64, ServiceError> {
    if !path.exists() {
        return Ok(0);
    }
    Ok(fs::metadata(path)?.len())
}

pub(super) fn ensure_file_exists(path: &Path) -> Result<(), ServiceError> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, [])?;
    Ok(())
}

pub(super) fn parse_first_recorded_pid(text: &str) -> Option<u32> {
    for line in text.lines() {
        if let Some(index) = line.find("PID:") {
            let digits = line[index + 4..]
                .chars()
                .skip_while(|ch| ch.is_whitespace())
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>();
            if let Ok(pid) = digits.parse::<u32>() {
                return Some(pid);
            }
        }
        if line.contains("Recording process ") {
            if let Some(open) = line.rfind('(') {
                if let Some(close) = line[open + 1..].find(')') {
                    let value = &line[open + 1..open + 1 + close];
                    if let Ok(pid) = value.parse::<u32>() {
                        return Some(pid);
                    }
                }
            }
        }
    }
    None
}

pub(super) fn recorder_error_summary(
    stdout_path: &Path,
    stderr_path: &Path,
    exit_code: Option<i32>,
) -> String {
    let stderr = read_text_lossy(stderr_path);
    if !stderr.trim().is_empty() {
        return format!(
            "TTD recorder exited with code {:?}: {}",
            exit_code,
            last_non_empty_line(&stderr)
        );
    }
    let stdout = read_text_lossy(stdout_path);
    if !stdout.trim().is_empty() {
        return format!(
            "TTD recorder exited with code {:?}: {}",
            exit_code,
            last_non_empty_line(&stdout)
        );
    }
    format!("TTD recorder exited with code {:?}", exit_code)
}

pub(super) fn read_text_lossy(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

pub(super) fn last_non_empty_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub(super) enum PreparedLaunch {
    #[cfg(windows)]
    Suspended(suspended_process::SuspendedProcess),
    #[cfg(not(windows))]
    Direct { pid: u32 },
}

impl PreparedLaunch {
    pub(super) fn create(executable: &Path, args: &[String]) -> Result<Self, ServiceError> {
        #[cfg(windows)]
        {
            suspended_process::SuspendedProcess::create(executable, args)
                .map(Self::Suspended)
                .map_err(ServiceError::Io)
        }

        #[cfg(not(windows))]
        {
            let child = Command::new(executable).args(args).spawn()?;
            Ok(Self::Direct { pid: child.id() })
        }
    }

    pub(super) fn pid(&self) -> u32 {
        match self {
            #[cfg(windows)]
            Self::Suspended(process) => process.pid(),
            #[cfg(not(windows))]
            Self::Direct { pid } => *pid,
        }
    }

    pub(super) fn resume(self) -> Result<(), ServiceError> {
        match self {
            #[cfg(windows)]
            Self::Suspended(process) => process.resume().map_err(ServiceError::Io),
            #[cfg(not(windows))]
            Self::Direct { .. } => Ok(()),
        }
    }
}

pub(super) fn write_json_file(path: &Path, value: &Value) -> Result<u64, ServiceError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, &bytes)?;
    Ok(bytes.len() as u64)
}

pub(super) fn write_jsonl_file(path: &Path, value: &Value) -> Result<u64, ServiceError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&bytes)?;
    Ok(bytes.len() as u64)
}
