use super::*;

impl ServiceHost {
    pub(super) fn recording_start(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: RecordingStartParams = parse_params(params)?;
        let request = StartRecording {
            target: params.target,
            presets: params.presets,
            cpu_sampling: params.cpu_sampling,
        }
        .validate()?;
        let recording_id = next_recording_ref();
        let mut cpu_active_guard = if request.cpu_sampling.is_some() {
            Some(self.start_cpu_recording(recording_id.clone())?)
        } else {
            None
        };
        let operation_id = next_operation_ref();
        let workspace = ensure_project_workspace(&params.project_root)?;
        let artifact_dir = workspace.ensure_recording_artifact_dir(&recording_id.id)?;
        let now = Timestamp::now();
        let trace_path = artifact_dir.join("trace.etl");
        let trace_session_name = etw_session_name(&recording_id);
        let trace_preset_flags = etw_preset_flags(&request.presets);
        let trace_start = self.etw_controller.start(
            &trace_session_name,
            &trace_path,
            trace_preset_flags,
            request.cpu_sampling,
        );
        let (mut trace_session, trace_start_error) = match trace_start {
            Ok(session) => (Some(session), None),
            Err(error) if request.cpu_sampling.is_some() => {
                return Err(ServiceError::Rpc(format!(
                    "CPU sampling ETW session failed to start: {error}"
                )));
            }
            Err(error) => (None, Some(error.to_string())),
        };
        let (trace_stack_status, trace_stack_status_error) = match trace_session.as_ref() {
            Some(session) => match session.stack_trace_status() {
                Ok(status) => (Some(status), None),
                Err(error) => (None, Some(error.to_string())),
            },
            None => (None, None),
        };
        let (cpu_sampling_status, cpu_sampling_status_error) = match trace_session.as_ref() {
            Some(session) if request.cpu_sampling.is_some() => {
                match session.cpu_sampling_status() {
                    Ok(status) if status.enabled && status.stack_enabled => (Some(status), None),
                    Ok(status) => {
                        if let Some(session) = trace_session.take() {
                            let _ = session.stop();
                        }
                        return Err(ServiceError::Rpc(format!(
                            "CPU sampling ETW session did not enable SampledProfile StackWalk: {status:?}"
                        )));
                    }
                    Err(error) => {
                        if let Some(session) = trace_session.take() {
                            let _ = session.stop();
                        }
                        return Err(ServiceError::Rpc(format!(
                            "CPU sampling status query failed: {error}"
                        )));
                    }
                }
            }
            _ => (None, None),
        };
        let prepared_target = match prepare_recording_target(&request.target) {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Some(session) = trace_session {
                    let _ = session.stop();
                }
                return Err(error);
            }
        };
        let root_pid = Some(prepared_target.root_pid());
        let trace_consumer_error = match trace_session.as_mut() {
            Some(session) => match session.start_realtime_consumer(
                artifact_dir.join("events"),
                trace_preset_flags,
                root_pid,
            ) {
                Ok(()) => None,
                Err(error) => Some(error.to_string()),
            },
            None => None,
        };
        if request.cpu_sampling.is_some() {
            if let Some(error) = trace_consumer_error.as_deref() {
                if let Some(session) = trace_session.take() {
                    let _ = session.stop();
                }
                return Err(ServiceError::Rpc(format!(
                    "CPU sampling realtime consumer failed to start: {error}"
                )));
            }
        }
        if let Err(error) = prepared_target.resume() {
            if let Some(session) = trace_session {
                let _ = session.stop();
            }
            return Err(error);
        }
        let trace_session = Arc::new(Mutex::new(trace_session));
        let writes = write_recording_start_artifacts(
            &artifact_dir,
            &recording_id,
            &operation_id,
            &request,
            root_pid,
            now,
            trace_start_error.as_deref(),
            trace_consumer_error.as_deref(),
            trace_stack_status,
            trace_stack_status_error.as_deref(),
            request.cpu_sampling,
            cpu_sampling_status,
            cpu_sampling_status_error.as_deref(),
        )?;
        let registered = register_worker_writes(&workspace, &operation_id, &writes)?;

        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "recording.start".to_string(),
            status: OperationStatus::Success,
            created_at: now,
            summary: "recording started".to_string(),
            artifacts: registered.artifacts.clone(),
            raw_output: registered.raw_output.clone(),
        })?;

        let recording = ManagedRecording {
            recording_id: recording_id.clone(),
            project_root: params.project_root,
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir,
            target: request.target,
            presets: request.presets,
            cpu_sampling: request.cpu_sampling,
            root_pid,
            state: RecordingState::Running,
            created_at: now,
            updated_at: now,
            last_operation: Some(operation_id.clone()),
            artifacts: registered.artifacts.clone(),
            trace_session,
            trace_start_error,
            trace_consumer_error,
            trace_stack_status,
            trace_stack_status_error,
            cpu_sampling_status,
            cpu_sampling_status_error,
            trace_stop_result: None,
            cpu_summary: None,
        };

        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "recording.start",
            None,
            "recording started",
        );
        operation.artifacts = registered.artifacts.clone();
        operation.raw_output = registered.raw_output.clone();

        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        state
            .recordings
            .insert(recording_id.id.as_str().to_string(), recording.clone());
        if let Some(guard) = cpu_active_guard.as_mut() {
            guard.commit();
        }

        recording_response(
            &recording,
            &operation_id,
            OperationStatus::Success,
            &registered,
        )
    }

    pub(super) fn recording_ttd(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: RecordingTtdParams = parse_params(params)?;
        let worker_identity = params.worker_identity;
        let request = RecordTtd {
            target: params.target,
            timeout_ms: params.timeout_ms,
            options: params.options,
        }
        .validate()?;
        let current_dir = ttd_launch_start_directory(&request.target)?;
        let recording_id = next_recording_ref();
        let operation_id = next_operation_ref();
        let _active_guard = self.start_ttd_recording(recording_id.clone())?;
        let workspace = ensure_project_workspace(&params.project_root)?;
        let artifact_dir = workspace.ensure_recording_artifact_dir(&recording_id.id)?;
        let traces_dir = artifact_dir.join("traces");
        fs::create_dir_all(&traces_dir)?;

        let ttd_exe = self.resolve_ttd_exe()?;
        let started = Instant::now();
        let started_at = Timestamp::now();
        let stdout_path = artifact_dir.join("recorder.stdout.txt");
        let stderr_path = artifact_dir.join("recorder.stderr.txt");
        let stop_stdout_path = artifact_dir.join("recorder-stop.stdout.txt");
        let stop_stderr_path = artifact_dir.join("recorder-stop.stderr.txt");
        let events_path = artifact_dir.join("events.jsonl");
        append_ttd_recording_event(
            &events_path,
            &recording_id,
            &operation_id,
            "ttd_recording_started",
            started_at,
            json!({
                "target": request.target,
                "timeout_ms": request.timeout_ms,
                "worker_identity": worker_identity,
                "ttd_exe": ttd_exe,
            }),
            None,
        )?;

        let args = build_ttd_args(&request.target, &request.options, &traces_dir);
        append_ttd_recording_event(
            &events_path,
            &recording_id,
            &operation_id,
            "recorder_starting",
            Timestamp::now(),
            json!({
                "args": args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                "current_dir": current_dir.as_ref(),
            }),
            None,
        )?;

        let timeout_stop_target = ttd_stop_target(&request.target, None);
        let recorder_exit = self.ttd_runner.run(TtdRecorderInvocation {
            ttd_exe: ttd_exe.clone(),
            args,
            current_dir,
            timeout: Duration::from_millis(request.timeout_ms),
            stdout_path: stdout_path.clone(),
            stderr_path: stderr_path.clone(),
            worker_identity,
            timeout_stop: timeout_stop_target
                .clone()
                .map(|stop_target| TtdTimeoutStop {
                    stop_target,
                    prefer_recorded_pid: matches!(&request.target, TtdTarget::Launch { .. }),
                    stdout_path: stop_stdout_path.clone(),
                    stderr_path: stop_stderr_path.clone(),
                    timeout: Duration::from_secs(15),
                    recorder_exit_timeout: Duration::from_secs(15),
                }),
        });
        ensure_file_exists(&stdout_path)?;
        ensure_file_exists(&stderr_path)?;

        let mut warnings = Vec::new();
        let mut error = None;
        let mut recorder_exit_code = None;
        let mut timed_out = false;
        let mut recorder_stop = None;
        let mut recorder_killed_after_timeout = false;
        match recorder_exit {
            Ok(exit) => {
                recorder_exit_code = exit.exit_code;
                timed_out = exit.timed_out;
                recorder_stop = exit.stop;
                recorder_killed_after_timeout = exit.killed_after_timeout;
                append_ttd_recording_event(
                    &events_path,
                    &recording_id,
                    &operation_id,
                    "recorder_finished",
                    Timestamp::now(),
                    json!({
                        "exit_code": exit.exit_code,
                        "timed_out": exit.timed_out,
                    }),
                    None,
                )?;
            }
            Err(run_error) => {
                error = Some(format!("start TTD recorder failed: {run_error}"));
                append_ttd_recording_event(
                    &events_path,
                    &recording_id,
                    &operation_id,
                    "recorder_failed",
                    Timestamp::now(),
                    json!({}),
                    error.clone(),
                )?;
            }
        }

        let target_pid = parse_first_recorded_pid(
            &(read_text_lossy(&stdout_path) + "\n" + read_text_lossy(&stderr_path).as_str()),
        );
        let mut stop_stdout_registered = false;
        let mut stop_stderr_registered = false;
        if timed_out {
            if recorder_killed_after_timeout {
                warnings
                    .push("TTD recorder did not exit after stop and was terminated".to_string());
            }
            append_ttd_recording_event(
                &events_path,
                &recording_id,
                &operation_id,
                "timeout_reached",
                Timestamp::now(),
                json!({}),
                None,
            )?;
            if let Some(stop) = recorder_stop {
                ensure_file_exists(&stop_stdout_path)?;
                ensure_file_exists(&stop_stderr_path)?;
                stop_stdout_registered = true;
                stop_stderr_registered = true;
                if let Some(stop_error) = stop.error {
                    let warning = format!("TTD stop failed after timeout: {stop_error}");
                    warnings.push(warning.clone());
                    append_ttd_recording_event(
                        &events_path,
                        &recording_id,
                        &operation_id,
                        "recorder_stop_failed",
                        Timestamp::now(),
                        json!({ "stop_target": stop.stop_target.to_string_lossy() }),
                        Some(warning),
                    )?;
                } else {
                    if stop.timed_out {
                        warnings.push("TTD stop command timed out".to_string());
                    }
                    append_ttd_recording_event(
                        &events_path,
                        &recording_id,
                        &operation_id,
                        "recorder_stop_finished",
                        Timestamp::now(),
                        json!({
                            "stop_target": stop.stop_target.to_string_lossy(),
                            "exit_code": stop.exit_code,
                            "timed_out": stop.timed_out,
                        }),
                        None,
                    )?;
                }
            } else {
                let message = match timeout_stop_target {
                    Some(stop_target) => format!(
                        "TTD recorder timed out, but stop was not attempted for {}",
                        stop_target.to_string_lossy()
                    ),
                    None => {
                        "TTD recorder timed out, but DbgAtlas could not determine a stop target"
                            .to_string()
                    }
                };
                warnings.push(message);
            }
        }

        let discovered = discover_ttd_artifacts(&traces_dir)?;
        let status = if error.is_some() {
            "failed"
        } else if recorder_exit_code.is_some_and(|code| code != 0) {
            error = Some(recorder_error_summary(
                &stdout_path,
                &stderr_path,
                recorder_exit_code,
            ));
            "failed"
        } else if discovered.traces.is_empty() {
            error = Some("TTD recorder completed but no .run trace was created".to_string());
            "failed"
        } else if timed_out {
            "timed_out"
        } else {
            "completed"
        };
        let operation_status = if status == "failed" {
            OperationStatus::Failed
        } else {
            OperationStatus::Success
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        let stopped_at = Timestamp::now();
        let metadata = ttd_recording_metadata_json(TtdRecordingMetadata {
            recording_id: &recording_id,
            operation_id: &operation_id,
            request: &request,
            status,
            operation_status: &operation_status,
            target_pid,
            recorder_exit_code,
            timed_out,
            started_at,
            stopped_at,
            duration_ms,
            ttd_exe: &ttd_exe,
            worker_identity,
            discovered: &discovered,
            warnings: &warnings,
            error: error.as_deref(),
        });
        let metadata_len = write_json_file(&artifact_dir.join("recording.json"), &metadata)?;
        append_ttd_recording_event(
            &events_path,
            &recording_id,
            &operation_id,
            "recording_completed",
            stopped_at,
            json!({
                "status": status,
                "operation_status": operation_status,
                "trace_count": discovered.traces.len(),
                "duration_ms": duration_ms,
            }),
            error.clone(),
        )?;

        let writes = ttd_recording_writes(
            &recording_id,
            &artifact_dir,
            metadata_len,
            stop_stdout_registered,
            stop_stderr_registered,
            &discovered,
        )?;
        let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
        let trace_artifacts = ttd_trace_artifact_results(&workspace, &writes, &registered);
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "recording.ttd".to_string(),
            status: operation_status.clone(),
            created_at: started_at,
            summary: format!("TTD recording {status}"),
            artifacts: registered.artifacts.clone(),
            raw_output: registered.raw_output.clone(),
        })?;

        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "recording.ttd",
            None,
            format!("TTD recording {status}"),
        );
        if operation_status == OperationStatus::Failed {
            operation.status = ServiceOperationStatus::Failed;
        }
        operation.artifacts = registered.artifacts.clone();
        operation.raw_output = registered.raw_output.clone();
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);

        Ok(json!({
            "recording_id": recording_id,
            "state": status,
            "target": request.target,
            "mode": request.target.mode(),
            "worker_identity": worker_identity,
            "operation_id": operation_id,
            "operation_status": operation_status,
            "target_pid": target_pid,
            "recorder_exit_code": recorder_exit_code,
            "duration_ms": duration_ms,
            "primary_trace_path": discovered.traces.first(),
            "trace_paths": discovered.traces,
            "trace_index_paths": discovered.trace_indexes,
            "trace_artifacts": trace_artifacts,
            "artifact_refs": registered.artifacts,
            "raw_output_ref": registered.raw_output,
            "warnings": warnings,
            "error": error,
            "operation": {
                "status": operation_status,
                "artifact_refs": registered.artifacts,
                "raw_output_ref": registered.raw_output,
            }
        }))
    }

    pub(super) fn recording_status(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: RecordingParams = parse_params(params)?;
        let state = self.lock_state()?;
        let recording = state
            .recordings
            .get(params.recording_id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::RecordingNotFound(params.recording_id.to_string()))?;
        Ok(recording_status_response(&recording))
    }

    pub(super) fn recording_stop(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_recording(params, RecordingFinishMode::Stop)
    }

    pub(super) fn recording_cancel(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_recording(params, RecordingFinishMode::Cancel)
    }

    pub(super) fn recording_kill(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_recording(params, RecordingFinishMode::Kill)
    }

    pub(super) fn finish_recording(
        &self,
        params: Option<Value>,
        mode: RecordingFinishMode,
    ) -> Result<Value, ServiceError> {
        let params: RecordingParams = parse_params(params)?;
        let mut recording = {
            let mut state = self.lock_state()?;
            let recording = state
                .recordings
                .get_mut(params.recording_id.id.as_str())
                .ok_or_else(|| ServiceError::RecordingNotFound(params.recording_id.to_string()))?;
            if recording.state != RecordingState::Running {
                return Err(ServiceError::RecordingAlreadyTerminal(
                    recording.recording_id.to_string(),
                ));
            }
            recording.state = RecordingState::Stopping;
            recording.updated_at = Timestamp::now();
            recording.clone()
        };

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&recording.internal_workspace_root)?;
        let artifact_dir = recording.artifact_dir.clone();
        let now = Timestamp::now();
        let (state, status, capability, summary, writes) = match mode {
            RecordingFinishMode::Stop => {
                let writes = write_recording_stop_artifacts(
                    &artifact_dir,
                    &mut recording,
                    &operation_id,
                    now,
                )?;
                (
                    RecordingState::Stopped,
                    OperationStatus::Success,
                    "recording.stop",
                    "recording stopped",
                    writes,
                )
            }
            RecordingFinishMode::Cancel => (
                RecordingState::Canceled,
                OperationStatus::Canceled,
                "recording.cancel",
                "recording canceled",
                write_recording_terminal_trace_artifacts(
                    &artifact_dir,
                    &mut recording,
                    &operation_id,
                    now,
                    "canceled",
                )?,
            ),
            RecordingFinishMode::Kill => (
                RecordingState::Killed,
                OperationStatus::Failed,
                "recording.kill",
                "recording killed",
                write_recording_terminal_trace_artifacts(
                    &artifact_dir,
                    &mut recording,
                    &operation_id,
                    now,
                    "killed",
                )?,
            ),
        };
        let registered = register_worker_writes(&workspace, &operation_id, &writes)?;
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: capability.to_string(),
            status: status.clone(),
            created_at: now,
            summary: summary.to_string(),
            artifacts: registered.artifacts.clone(),
            raw_output: registered.raw_output.clone(),
        })?;

        recording.state = state;
        recording.updated_at = now;
        recording.last_operation = Some(operation_id.clone());
        recording.artifacts = registered.artifacts.clone();

        let mut operation =
            ServiceOperation::success(operation_id.clone(), capability, None, summary);
        if status == OperationStatus::Canceled {
            operation.status = ServiceOperationStatus::Canceled;
        }
        if status == OperationStatus::Failed {
            operation.status = ServiceOperationStatus::Failed;
        }
        operation.artifacts = registered.artifacts.clone();
        operation.raw_output = registered.raw_output.clone();

        let mut state = self.lock_state()?;
        if state.active_cpu_recording.as_ref() == Some(&recording.recording_id) {
            state.active_cpu_recording = None;
        }
        state.recordings.insert(
            recording.recording_id.id.as_str().to_string(),
            recording.clone(),
        );
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);

        recording_response(&recording, &operation_id, status, &registered)
    }
}

pub(super) fn recording_response(
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    Ok(json!({
        "recording_id": recording.recording_id,
        "state": recording.state,
        "target": recording.target,
        "root_pid": recording.root_pid,
        "presets": recording.presets,
        "cpu_sampling": recording.cpu_sampling,
        "cpu_summary": recording.cpu_summary,
        "operation_id": operation_id,
        "operation_status": status,
        "artifact_refs": writes.artifacts,
        "raw_output_ref": writes.raw_output,
        "operation": {
            "status": status,
            "artifact_refs": writes.artifacts,
            "raw_output_ref": writes.raw_output,
        }
    }))
}

pub(super) fn recording_status_response(recording: &ManagedRecording) -> Value {
    json!({
        "recording_id": recording.recording_id,
        "state": recording.state,
        "target": recording.target,
        "root_pid": recording.root_pid,
        "presets": recording.presets,
        "cpu_sampling": recording.cpu_sampling,
        "cpu_summary": recording.cpu_summary,
        "created_at": recording.created_at,
        "updated_at": recording.updated_at,
        "last_operation": recording.last_operation,
        "artifact_refs": recording.artifacts,
    })
}
