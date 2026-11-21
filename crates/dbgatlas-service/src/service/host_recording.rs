use super::*;

#[derive(Clone)]
pub(super) struct RemoteRecording {
    pub worker: WorkerHandle,
    pub session_id: SessionRef,
    pub workspace_root: PathBuf,
    pub artifact_dir: PathBuf,
    pub request_lock: Arc<Mutex<()>>,
    pub result: Value,
    pub terminal: bool,
    pub cpu: bool,
    pub ttd: bool,
}

impl ServiceHost {
    pub(super) fn recording_start(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.open_recording(params, false)
    }
    pub(super) fn recording_ttd(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.open_recording(params, true)
    }
    fn open_recording(&self, params: Option<Value>, ttd: bool) -> Result<Value, ServiceError> {
        let params = params.unwrap_or_else(|| json!({}));
        // Validate before accepting or starting a worker.
        let (project_root, target, cpu) = if ttd {
            let p: RecordingTtdParams = serde_json::from_value(params.clone())?;
            RecordTtd {
                target: p.target.clone(),
                timeout_ms: p.timeout_ms,
                options: p.options,
            }
            .validate()?;
            (p.project_root, serde_json::to_value(p.target)?, false)
        } else {
            let p: RecordingStartParams = serde_json::from_value(params.clone())?;
            let r = StartRecording {
                target: p.target,
                presets: p.presets,
                cpu_sampling: p.cpu_sampling,
            }
            .validate()?;
            (
                p.project_root,
                serde_json::to_value(r.target)?,
                r.cpu_sampling.is_some(),
            )
        };
        let recording_id = next_recording_ref();
        let session_id = SessionRef::new(recording_id.id.clone());
        let operation_id = next_operation_ref();
        let mut cpu_guard = if cpu {
            Some(self.start_cpu_recording(recording_id.clone())?)
        } else {
            None
        };
        let ttd_guard = if ttd {
            Some(self.start_ttd_recording(recording_id.clone())?)
        } else {
            None
        };
        let workspace = self.ensure_project_workspace(&project_root)?;
        let artifact_dir = workspace.ensure_recording_artifact_dir(&recording_id.id)?;
        let method = if ttd {
            "recording.ttd"
        } else {
            "recording.start"
        };
        self.open_session_record(
            &session_id,
            &workspace,
            dbgatlas_model::SessionKind::Recording,
            target,
            &operation_id,
            method,
            params.clone(),
        )?;
        let worker = match self.supervisor.create_worker(WorkerCreateRequest {
            session_id: session_id.clone(),
            project_root,
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: artifact_dir.clone(),
            startup_timeout_ms: DEFAULT_DEBUG_SESSION_STARTUP_TIMEOUT_MS,
            dbgeng_dirs: self.capabilities.dbgeng_dirs.clone(),
            identity: None,
        }) {
            Ok(worker) => worker,
            Err(error) => {
                self.set_session_terminal(&session_id, true, true, Some(error.to_string()))?;
                self.append_operation_result(
                    &workspace,
                    &OperationRecord {
                        details: Default::default(),
                        operation_id: operation_id.clone(),
                        adapter_id: "service".into(),
                        capability: method.into(),
                        status: OperationStatus::Failed,
                        created_at: Timestamp::now(),
                        summary: error.to_string(),
                        artifacts: vec![],
                        raw_output: None,
                    },
                )?;
                return Err(error);
            }
        };
        let remote = RemoteRecording {
            worker,
            session_id: session_id.clone(),
            workspace_root: workspace.root().into(),
            artifact_dir,
            request_lock: Arc::new(Mutex::new(())),
            result: json!({"session_id":session_id,"recording_id":session_id,"state":"opening","operation_id":operation_id,"artifact_refs":[]}),
            terminal: false,
            cpu,
            ttd,
        };
        let _request_guard = remote
            .request_lock
            .lock()
            .map_err(|_| ServiceError::SessionBusy(session_id.to_string()))?;
        self.lock_state()?
            .remote_recordings
            .insert(session_id.to_string(), remote.clone());
        // Keep global reservations until cleanup is confirmed, including failed starts.
        if let Some(guard) = cpu_guard.as_mut() {
            guard.commit();
        }
        if let Some(mut guard) = ttd_guard {
            guard.committed = true;
        }
        if let Err(error) =
            self.bind_session_worker(&session_id, &remote.worker, if ttd { "ttd" } else { "etw" })
        {
            let cleanup = self.supervisor.kill_worker(&remote.worker);
            let _ = self.set_session_terminal(
                &session_id,
                true,
                cleanup.is_ok(),
                Some(error.to_string()),
            );
            if cleanup.is_ok() {
                self.release_recording_slot(&remote)?;
            }
            return Err(error);
        }
        if let Err(error) = self.start_open_operation(
            &workspace,
            &operation_id,
            &session_id,
            &remote.worker,
            if ttd { "ttd" } else { "etw" },
        ) {
            let cleanup = self.supervisor.kill_worker(&remote.worker);
            self.set_session_terminal(&session_id, true, cleanup.is_ok(), Some(error.to_string()))?;
            if cleanup.is_ok() {
                self.release_recording_slot(&remote)?;
            }
            return Err(error);
        }
        let response = self.request_recording(&remote, &operation_id, method, params);
        self.finish_recording_response(&remote, &operation_id, method, response, ttd)
    }
    fn request_recording(
        &self,
        recording: &RemoteRecording,
        operation_id: &OperationRef,
        method: &str,
        params: Value,
    ) -> Result<WorkerResponse, ServiceError> {
        #[cfg(test)]
        {
            if self.supervisor.is_mock() {
                let mut executor = self
                    .lock_state()?
                    .recording_executors
                    .remove(recording.session_id.id.as_str())
                    .unwrap_or_else(|| {
                        RecordingExecutor::new(
                            recording.session_id.clone(),
                            self.capabilities.ttd_dir.clone(),
                        )
                        .with_adapters(self.etw_controller.clone(), self.ttd_runner.clone())
                    });
                let response = executor.execute(operation_id.clone(), method, params);
                self.lock_state()?
                    .recording_executors
                    .insert(recording.session_id.to_string(), executor);
                return Ok(response);
            }
        }
        self.supervisor.request_worker(
            &recording.worker,
            WorkerRequest::Recording {
                session_id: recording.session_id.clone(),
                operation_id: operation_id.clone(),
                action: method.into(),
                params,
                ttd_dir: self.capabilities.ttd_dir.clone(),
            },
        )
    }
    fn finish_recording_response(
        &self,
        recording: &RemoteRecording,
        operation_id: &OperationRef,
        method: &str,
        response: Result<WorkerResponse, ServiceError>,
        terminal: bool,
    ) -> Result<Value, ServiceError> {
        let workspace = self.history.lookup(&recording.workspace_root)?;
        let (mut result, writes) = match response {
            Ok(WorkerResponse::Recording { result, writes }) => (result, writes),
            other => {
                let status = if matches!(other, Ok(WorkerResponse::Failed { .. })) {
                    OperationStatus::Failed
                } else {
                    OperationStatus::Unknown
                };
                let message = format!("recording execution unavailable: {other:?}");
                let cleanup = self.supervisor.kill_worker(&recording.worker);
                self.set_session_terminal(
                    &recording.session_id,
                    true,
                    cleanup.is_ok(),
                    Some(message.clone()),
                )?;
                if let Some(remote) = self
                    .lock_state()?
                    .remote_recordings
                    .get_mut(recording.session_id.id.as_str())
                {
                    remote.terminal = true;
                }
                self.append_operation_result(
                    &workspace,
                    &OperationRecord {
                        details: Default::default(),
                        operation_id: operation_id.clone(),
                        adapter_id: "service".into(),
                        capability: method.into(),
                        status: status.clone(),
                        created_at: Timestamp::now(),
                        summary: message.clone(),
                        artifacts: vec![],
                        raw_output: None,
                    },
                )?;
                self.finish_operation_in_memory(
                    operation_id,
                    status,
                    message.clone(),
                    vec![],
                    None,
                )?;
                if cleanup.is_ok() {
                    self.release_recording_slot(recording)?;
                }
                return Err(ServiceError::Worker(message));
            }
        };
        let status: OperationStatus = serde_json::from_value(result["operation_status"].clone())?;
        // Execution facts survive a separate output registration failure.
        self.finish_operation_in_memory(
            operation_id,
            status.clone(),
            format!("{method} executed"),
            vec![],
            None,
        )?;
        let (registered, output_error) =
            match register_worker_writes(&workspace, operation_id, &writes) {
                Ok(registered) => (registered, None),
                Err(error) => (RegisteredWorkerWrites::default(), Some(error.to_string())),
            };
        replace_worker_output_refs(&mut result, &registered.artifacts);
        if output_error.is_some() {
            result["artifact_refs"] = json!([]);
            result["output_error"] = json!(output_error);
        }
        result["session_id"] = serde_json::to_value(&recording.session_id)?;
        result["recording_id"] = serde_json::to_value(&recording.session_id)?;
        self.finish_operation_in_memory(
            operation_id,
            status.clone(),
            format!("{method} completed"),
            registered.artifacts.clone(),
            registered.raw_output.clone(),
        )?;
        if let Some(op) = self
            .lock_state()?
            .operations
            .get_mut(operation_id.id.as_str())
        {
            op.details.output_error = output_error.clone();
        }
        let journal = self.append_operation_result(
            &workspace,
            &OperationRecord {
                details: dbgatlas_model::OperationDetails {
                    output_error: output_error.clone(),
                    ..Default::default()
                },
                operation_id: operation_id.clone(),
                adapter_id: "service".into(),
                capability: method.into(),
                status: status.clone(),
                created_at: Timestamp::now(),
                summary: format!("{method} completed"),
                artifacts: registered.artifacts.clone(),
                raw_output: registered.raw_output.clone(),
            },
        );
        if let Err(error) = journal {
            let message =
                format!("execution completed; operation journal failed: {error}; do not replay");
            result["output_error"] = json!(message);
            if let Some(op) = self
                .lock_state()?
                .operations
                .get_mut(operation_id.id.as_str())
            {
                op.details.output_error = Some(message);
            }
        }
        let already_terminated = self
            .lock_state()?
            .remote_recordings
            .get(recording.session_id.id.as_str())
            .is_some_and(|r| r.terminal);
        if already_terminated {
            return Err(ServiceError::Worker("recording was concurrently terminated; late response retained without reactivating session".into()));
        }
        if terminal {
            let cleanup = if status == OperationStatus::Failed {
                self.supervisor.kill_worker(&recording.worker)
            } else {
                match self.supervisor.request_worker(
                    &recording.worker,
                    WorkerRequest::CloseSession {
                        session_id: recording.session_id.clone(),
                    },
                ) {
                    Ok(WorkerResponse::Ok { .. }) => {
                        self.supervisor.close_worker(&recording.worker)
                    }
                    other => Err(ServiceError::Worker(format!(
                        "recording close failed: {other:?}"
                    ))),
                }
            };
            self.set_session_terminal(
                &recording.session_id,
                cleanup.is_err() || status == OperationStatus::Failed,
                cleanup.is_ok(),
                cleanup.as_ref().err().map(ToString::to_string),
            )?;
            if cleanup.is_ok() {
                self.release_recording_slot(recording)?;
            } else {
                result["cleanup_error"] = json!(cleanup.unwrap_err().to_string());
            }
        }
        let mut state = self.lock_state()?;
        if let Some(remote) = state
            .remote_recordings
            .get_mut(recording.session_id.id.as_str())
        {
            if remote.terminal {
                return Err(ServiceError::Worker(
                    "recording was concurrently terminated".into(),
                ));
            }
            remote.result = result.clone();
            remote.terminal = terminal;
        }
        if let Some(session) = state.summaries.get_mut(recording.session_id.id.as_str()) {
            if !terminal {
                session.lifecycle = dbgatlas_model::SessionLifecycle::Active;
            }
            session.artifacts = registered.artifacts;
            session.last_operation_id = Some(operation_id.clone());
            session.context = json!({"recording_state":result["state"]});
            if let Some(value) = result.get("target_process").filter(|v| !v.is_null()) {
                let mut process: dbgatlas_model::ProcessInstance =
                    serde_json::from_value(value.clone())?;
                process.environment_id = session.control_environment.id.clone();
                session.target.process = Some(process);
            }
            self.persist_session(session)?;
        }
        Ok(result)
    }
    pub(super) fn release_recording_slot(
        &self,
        recording: &RemoteRecording,
    ) -> Result<(), ServiceError> {
        let mut state = self.lock_state()?;
        if recording.cpu
            && state
                .active_cpu_recording
                .as_ref()
                .is_some_and(|id| id.id == recording.session_id.id)
        {
            state.active_cpu_recording = None;
        }
        if recording.ttd
            && state
                .active_ttd_recording
                .as_ref()
                .is_some_and(|id| id.id == recording.session_id.id)
        {
            state.active_ttd_recording = None;
        }
        Ok(())
    }
    pub(super) fn recording_status(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: RecordingParams = parse_params(params)?;
        let state = self.lock_state()?;
        let recording = state
            .remote_recordings
            .get(params.recording_id.id.as_str())
            .ok_or_else(|| ServiceError::RecordingNotFound(params.recording_id.to_string()))?;
        let mut result = recording.result.clone();
        if let Some(session) = state.summaries.get(params.recording_id.id.as_str()) {
            result["last_operation"] = serde_json::to_value(&session.last_operation_id)?;
            result["created_at"] = serde_json::to_value(session.created_at)?;
            result["updated_at"] = serde_json::to_value(session.updated_at)?;
        }
        Ok(result)
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
        let recording = self
            .lock_state()?
            .remote_recordings
            .get(params.recording_id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::RecordingNotFound(params.recording_id.to_string()))?;
        let force = matches!(mode, RecordingFinishMode::Kill);
        let cleanup_complete = self
            .lock_state()?
            .summaries
            .get(recording.session_id.id.as_str())
            .is_some_and(|s| s.cleanup.complete);
        if recording.terminal && (!force || cleanup_complete) {
            return Err(ServiceError::RecordingAlreadyTerminal(
                params.recording_id.to_string(),
            ));
        }
        let _guard = if force {
            None
        } else {
            Some(
                recording
                    .request_lock
                    .try_lock()
                    .map_err(|_| ServiceError::SessionBusy(recording.session_id.to_string()))?,
            )
        };
        if !force
            && self
                .lock_state()?
                .remote_recordings
                .get(recording.session_id.id.as_str())
                .is_some_and(|r| r.terminal)
        {
            return Err(ServiceError::RecordingAlreadyTerminal(
                params.recording_id.to_string(),
            ));
        }
        let method = match mode {
            RecordingFinishMode::Stop => "recording.stop",
            RecordingFinishMode::Cancel => "recording.cancel",
            RecordingFinishMode::Kill => "recording.kill",
        };
        let operation_id = next_operation_ref();
        let workspace = self.history.lookup(&recording.workspace_root)?;
        self.begin_operation_record(
            &workspace,
            &operation_id,
            &recording.session_id,
            method,
            json!({"recording_id":params.recording_id}),
        )?;
        if force {
            if let Some(remote) = self
                .lock_state()?
                .remote_recordings
                .get_mut(recording.session_id.id.as_str())
            {
                remote.terminal = true;
            }
            if let Err(error) = self.supervisor.kill_worker(&recording.worker) {
                self.set_session_terminal(
                    &recording.session_id,
                    true,
                    false,
                    Some(error.to_string()),
                )?;
                self.finish_operation_in_memory(
                    &operation_id,
                    OperationStatus::Failed,
                    error.to_string(),
                    vec![],
                    None,
                )?;
                self.append_operation_result(
                    &workspace,
                    &OperationRecord {
                        details: Default::default(),
                        operation_id,
                        adapter_id: "service".into(),
                        capability: method.into(),
                        status: OperationStatus::Failed,
                        created_at: Timestamp::now(),
                        summary: error.to_string(),
                        artifacts: vec![],
                        raw_output: None,
                    },
                )?;
                return Err(error);
            }
            self.set_session_terminal(
                &recording.session_id,
                true,
                true,
                Some("recording worker forcefully terminated; outputs may be incomplete".into()),
            )?;
            self.release_recording_slot(&recording)?;
            let metadata_path = recording.artifact_dir.join("recording.json");
            let mut metadata: Value = fs::read(&metadata_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_else(|| json!({}));
            metadata["state"] = json!("killed");
            metadata["output_complete"] = json!(false);
            metadata["operation_id"] = serde_json::to_value(&operation_id)?;
            let length = write_json_file(&metadata_path, &metadata)?;
            let registered = register_worker_writes(
                &workspace,
                &operation_id,
                &[WorkerArtifactWrite {
                    relative_path: PathBuf::from("artifacts/recordings")
                        .join(recording.session_id.id.as_str())
                        .join("recording.json"),
                    kind: "recording.metadata".into(),
                    role: None,
                    byte_len: length,
                    description: Some("forced recording termination; outputs incomplete".into()),
                }],
            )?;
            let mut result = recording.result.clone();
            result["artifact_refs"] = serde_json::to_value(&registered.artifacts)?;
            result["state"] = json!("killed");
            result["operation_status"] = json!("failed");
            result["operation_id"] = serde_json::to_value(&operation_id)?;
            result["warnings"] = json!(["forced termination; output completeness is unknown"]);
            let mut state = self.lock_state()?;
            if let Some(r) = state
                .remote_recordings
                .get_mut(recording.session_id.id.as_str())
            {
                r.terminal = true;
                r.result = result.clone();
            }
            drop(state);
            self.append_operation_result(
                &workspace,
                &OperationRecord {
                    details: Default::default(),
                    operation_id: operation_id.clone(),
                    adapter_id: "service".into(),
                    capability: method.into(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: "recording forced closed".into(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: None,
                },
            )?;
            self.finish_operation_in_memory(
                &operation_id,
                ServiceOperationStatus::Failed,
                "recording forced closed".into(),
                registered.artifacts,
                None,
            )?;
            return Ok(result);
        }
        let response = self.request_recording(
            &recording,
            &operation_id,
            method,
            json!({"recording_id":params.recording_id}),
        );
        self.finish_recording_response(&recording, &operation_id, method, response, true)
    }
}

fn replace_worker_output_refs(value: &mut Value, refs: &[ArtifactRef]) {
    match value {
        Value::String(text) => {
            if let Some(index) = text
                .strip_prefix("worker-output-")
                .and_then(|v| v.parse::<usize>().ok())
            {
                if let Some(reference) = refs.get(index) {
                    *text = reference.id.to_string();
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                replace_worker_output_refs(item, refs);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                replace_worker_output_refs(item, refs);
            }
        }
        _ => {}
    }
}
