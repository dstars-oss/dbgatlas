use super::*;

impl ServiceHost {
    pub(super) fn eval_command(
        &self,
        request: EvalDebugCommand,
        capability: &'static str,
    ) -> Result<Value, ServiceError> {
        let session = {
            let state = self.lock_state()?;
            state
                .sessions
                .get(request.session_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(request.session_id.to_string()))?
        };
        if !session.state.is_reusable() {
            return Err(ServiceError::SessionNotReusable(
                request.session_id.to_string(),
            ));
        }
        if session.capability != "debug" {
            return Err(ServiceError::Rpc(format!(
                "session {} is {}, expected debug",
                request.session_id, session.capability
            )));
        }
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = {
            let state = self.lock_state()?;
            state
                .sessions
                .get(request.session_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(request.session_id.to_string()))?
        };
        if !session.state.is_reusable() {
            return Err(ServiceError::SessionNotReusable(
                request.session_id.to_string(),
            ));
        }

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let operation = ServiceOperation::running(
            operation_id.clone(),
            capability,
            Some(session.session_id.clone()),
            format!("{capability} running"),
        );
        {
            let mut state = self.lock_state()?;
            state
                .operations
                .insert(operation_id.id.as_str().to_string(), operation);
        }

        let worker_response = self.request_worker_with_optional_timeout(
            &session,
            WorkerRequest::EvalDebugCommand {
                session_id: session.session_id.clone(),
                operation_id: operation_id.clone(),
                command: request.command.clone(),
                artifact_dir: session.artifact_dir.clone(),
            },
            &operation_id,
            request.timeout_ms,
        );

        self.finish_command_worker_response(
            &session,
            &workspace,
            operation_id,
            capability,
            request.command,
            worker_response,
        )
    }

    pub(super) fn add_symbols(&self, request: AddSymbolsRequest) -> Result<Value, ServiceError> {
        let session = self.reusable_debug_session(&request.session_id)?;
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = self.reusable_debug_session(&request.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let audit_command = if request.reload {
            format!(".sympath+ {}; .reload", request.symbol_path)
        } else {
            format!(".sympath+ {}", request.symbol_path)
        };
        let operation = ServiceOperation::running(
            operation_id.clone(),
            "debug.add_symbols",
            Some(session.session_id.clone()),
            "debug.add_symbols running",
        );
        {
            let mut state = self.lock_state()?;
            state
                .operations
                .insert(operation_id.id.as_str().to_string(), operation);
        }

        let worker_response = self.supervisor.request_worker(
            &session.worker,
            WorkerRequest::AddSymbols {
                session_id: session.session_id.clone(),
                operation_id: operation_id.clone(),
                symbol_path: request.symbol_path,
                reload: request.reload,
                artifact_dir: session.artifact_dir.clone(),
            },
        );

        self.finish_command_worker_response(
            &session,
            &workspace,
            operation_id,
            "debug.add_symbols",
            audit_command,
            worker_response,
        )
    }

    pub(super) fn read_memory(&self, request: ReadMemoryRequest) -> Result<Value, ServiceError> {
        let session = self.reusable_debug_session(&request.session_id)?;
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = self.reusable_debug_session(&request.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let audit_command = format!(
            "read_memory address={} length={}",
            request.address, request.length
        );
        let operation = ServiceOperation::running(
            operation_id.clone(),
            "debug.read_memory",
            Some(session.session_id.clone()),
            "debug.read_memory running",
        );
        {
            let mut state = self.lock_state()?;
            state
                .operations
                .insert(operation_id.id.as_str().to_string(), operation);
        }

        let worker_response = self.supervisor.request_worker(
            &session.worker,
            WorkerRequest::ReadMemory {
                session_id: session.session_id.clone(),
                operation_id: operation_id.clone(),
                address: request.address,
                length: request.length,
                artifact_dir: session.artifact_dir.clone(),
            },
        );

        self.finish_memory_worker_response(
            &session,
            &workspace,
            operation_id,
            audit_command,
            worker_response,
        )
    }

    pub(super) fn finish_command_worker_response(
        &self,
        session: &ManagedSession,
        workspace: &Workspace,
        operation_id: OperationRef,
        capability: &'static str,
        audit_command: String,
        worker_response: Result<WorkerResponse, ServiceError>,
    ) -> Result<Value, ServiceError> {
        match worker_response {
            Ok(WorkerResponse::DebugCommand { mut result, writes }) => {
                let registered = match register_worker_writes(workspace, &operation_id, &writes) {
                    Ok(registered) => registered,
                    Err(error) => {
                        self.record_debug_artifact_registration_failure(
                            session,
                            workspace,
                            &operation_id,
                            capability,
                            &audit_command,
                            &error,
                        )?;
                        return Err(error);
                    }
                };
                result.operation_id = Some(operation_id.clone());
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                if was_canceled {
                    result.warnings.push("operation was canceled".to_string());
                }
                let workspace_status = if was_canceled {
                    OperationStatus::Canceled
                } else {
                    OperationStatus::Success
                };
                let summary = if was_canceled {
                    format!("{capability} canceled")
                } else {
                    format!("{capability} completed by worker")
                };
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: workspace_status.clone(),
                    created_at: Timestamp::now(),
                    summary: summary.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: capability.to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: workspace_status.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                append_operation_diagnostic_log(
                    capability,
                    &session.session_id,
                    &operation_id,
                    &workspace_status,
                    registered.artifacts.len(),
                    registered.raw_output.as_ref(),
                    None,
                );
                finalize_debug_command_result(&mut result, &registered);
                let response = command_result_response(&result, workspace_status, &registered)?;

                let mut state = self.lock_state()?;
                if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
                    if !was_canceled {
                        operation.status = ServiceOperationStatus::Success;
                    }
                    operation.summary = summary;
                    operation.artifacts = registered.artifacts;
                    operation.raw_output = registered.raw_output;
                    operation.updated_at = Timestamp::now();
                    operation.events.push(ServiceEvent {
                        timestamp: Timestamp::now(),
                        kind: "output".to_string(),
                        message: result.result.value.clone(),
                    });
                }
                if let Some(session) = state.sessions.get_mut(session.session_id.id.as_str()) {
                    session.last_operation = Some(operation_id.clone());
                    session.updated_at = Timestamp::now();
                }

                Ok(response)
            }
            Ok(WorkerResponse::Failed {
                code,
                message,
                writes,
            }) => {
                let registered = match register_worker_writes(workspace, &operation_id, &writes) {
                    Ok(registered) => registered,
                    Err(error) => {
                        self.record_debug_artifact_registration_failure(
                            session,
                            workspace,
                            &operation_id,
                            capability,
                            &audit_command,
                            &error,
                        )?;
                        return Err(error);
                    }
                };
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: capability.to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: OperationStatus::Failed,
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    registered.artifacts.clone(),
                    registered.raw_output.clone(),
                )?;
                append_operation_diagnostic_log(
                    capability,
                    &session.session_id,
                    &operation_id,
                    &OperationStatus::Failed,
                    registered.artifacts.len(),
                    registered.raw_output.as_ref(),
                    Some(&format!("{code}: {message}")),
                );
                Err(ServiceError::Worker(format!("{code}: {message}")))
            }
            Ok(other) => {
                let message = format!("unexpected eval response: {other:?}");
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: capability.to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: OperationStatus::Failed,
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    Vec::new(),
                    None,
                )?;
                append_operation_diagnostic_log(
                    capability,
                    &session.session_id,
                    &operation_id,
                    &OperationStatus::Failed,
                    0,
                    None,
                    Some(&message),
                );
                Err(ServiceError::Worker(message))
            }
            Err(error) => {
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                let status = if was_canceled {
                    OperationStatus::Canceled
                } else {
                    OperationStatus::Failed
                };
                let final_output_path = session
                    .artifact_dir
                    .join("raw")
                    .join(format!("{}.txt", operation_id.id.as_str()));
                if let Err(cleanup_error) = cleanup_partial_result_write(&final_output_path) {
                    append_service_diagnostic_log(&format!(
                        "debug_partial_cleanup_failed session_id={} operation_id={} error={}",
                        sanitize_log_value(session.session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(&cleanup_error.to_string())
                    ));
                }
                let recovered = recover_full_result_write(
                    workspace,
                    &operation_id,
                    &final_output_path,
                    session_relative_path(
                        &session.session_id,
                        &format!("raw/{}.txt", operation_id.id.as_str()),
                    ),
                    "debug.raw_output",
                    "recovered debug raw command output",
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: capability.to_string(),
                    status: status.clone(),
                    created_at: Timestamp::now(),
                    summary: error.to_string(),
                    artifacts: recovered.artifacts.clone(),
                    raw_output: recovered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: capability.to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: status.clone(),
                    artifacts: recovered.artifacts.clone(),
                    raw_output: recovered.raw_output.clone(),
                })?;
                if !was_canceled {
                    self.finish_operation_in_memory(
                        &operation_id,
                        ServiceOperationStatus::Failed,
                        error.to_string(),
                        recovered.artifacts.clone(),
                        recovered.raw_output.clone(),
                    )?;
                }
                append_operation_diagnostic_log(
                    capability,
                    &session.session_id,
                    &operation_id,
                    &status,
                    recovered.artifacts.len(),
                    recovered.raw_output.as_ref(),
                    Some(&error.to_string()),
                );
                Err(error)
            }
        }
    }

    fn record_debug_artifact_registration_failure(
        &self,
        session: &ManagedSession,
        workspace: &Workspace,
        operation_id: &OperationRef,
        capability: &str,
        audit_command: &str,
        error: &ServiceError,
    ) -> Result<(), ServiceError> {
        let message = format!("worker artifact registration failed: {error}");
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: capability.to_string(),
            status: OperationStatus::Failed,
            created_at: Timestamp::now(),
            summary: message.clone(),
            artifacts: Vec::new(),
            raw_output: None,
        })?;
        workspace.append_command_audit(&CommandAuditRecord {
            operation_id: operation_id.clone(),
            session_id: Some(session.session_id.clone()),
            capability: capability.to_string(),
            command: audit_command.to_string(),
            created_at: Timestamp::now(),
            status: OperationStatus::Failed,
            artifacts: Vec::new(),
            raw_output: None,
        })?;
        self.finish_operation_in_memory(
            operation_id,
            ServiceOperationStatus::Failed,
            message.clone(),
            Vec::new(),
            None,
        )?;
        append_operation_diagnostic_log(
            capability,
            &session.session_id,
            operation_id,
            &OperationStatus::Failed,
            0,
            None,
            Some(&message),
        );
        Ok(())
    }

    pub(super) fn request_worker_with_optional_timeout(
        &self,
        session: &ManagedSession,
        request: WorkerRequest,
        operation_id: &OperationRef,
        timeout_ms: Option<u64>,
    ) -> Result<WorkerResponse, ServiceError> {
        let Some(timeout_ms) = timeout_ms else {
            return self.supervisor.request_worker(&session.worker, request);
        };
        let timeout = Duration::from_millis(timeout_ms);
        let supervisor = Arc::clone(&self.supervisor);
        let worker = session.worker.clone();
        let session_id = session.session_id.clone();
        let operation_id_for_thread = operation_id.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let response = supervisor.request_worker(&worker, request);
            let _ = sender.send(response);
        });

        match receiver.recv_timeout(timeout) {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                append_service_diagnostic_log(&format!(
                    "debug_eval_timeout session_id={} operation_id={} timeout_ms={}",
                    sanitize_log_value(session_id.id.as_str()),
                    sanitize_log_value(operation_id.id.as_str()),
                    timeout_ms
                ));
                if let Err(error) = self.supervisor.kill_worker(&session.worker) {
                    append_service_diagnostic_log(&format!(
                        "debug_eval_timeout_kill_failed session_id={} operation_id={} error={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(&error.to_string())
                    ));
                }
                self.mark_debug_session_error(&session_id)?;
                Err(ServiceError::Worker(format!(
                    "debug operation {} timed out after {} ms; debug worker was killed and the session must be recreated",
                    operation_id_for_thread, timeout_ms
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ServiceError::Worker(
                "worker request ended before returning a response".to_string(),
            )),
        }
    }

    pub(super) fn finish_memory_worker_response(
        &self,
        session: &ManagedSession,
        workspace: &Workspace,
        operation_id: OperationRef,
        audit_command: String,
        worker_response: Result<WorkerResponse, ServiceError>,
    ) -> Result<Value, ServiceError> {
        match worker_response {
            Ok(WorkerResponse::DebugMemory { mut result, writes }) => {
                let registered = register_worker_writes(workspace, &operation_id, &writes)?;
                result.operation_id = Some(operation_id.clone());
                result.memory = registered.memory.clone();
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                if was_canceled {
                    result.warnings.push("operation was canceled".to_string());
                }
                let status = if was_canceled {
                    OperationStatus::Canceled
                } else {
                    OperationStatus::Success
                };
                let summary = if was_canceled {
                    "debug.read_memory canceled"
                } else {
                    "debug.read_memory completed by worker"
                };
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: status.clone(),
                    created_at: Timestamp::now(),
                    summary: summary.to_string(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: "debug.read_memory".to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: status.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                append_operation_diagnostic_log(
                    "debug.read_memory",
                    &session.session_id,
                    &operation_id,
                    &status,
                    registered.artifacts.len(),
                    registered.raw_output.as_ref(),
                    None,
                );
                let response = memory_result_response(&result, status, &registered)?;

                let mut state = self.lock_state()?;
                if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
                    if !was_canceled {
                        operation.status = ServiceOperationStatus::Success;
                    }
                    operation.summary = summary.to_string();
                    operation.artifacts = registered.artifacts;
                    operation.raw_output = registered.raw_output;
                    operation.updated_at = Timestamp::now();
                }
                if let Some(session) = state.sessions.get_mut(session.session_id.id.as_str()) {
                    session.last_operation = Some(operation_id.clone());
                    session.updated_at = Timestamp::now();
                }

                Ok(response)
            }
            Ok(WorkerResponse::Failed {
                code,
                message,
                writes,
            }) => {
                let registered = register_worker_writes(workspace, &operation_id, &writes)?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: "debug.read_memory".to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: OperationStatus::Failed,
                    artifacts: registered.artifacts.clone(),
                    raw_output: registered.raw_output.clone(),
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    registered.artifacts.clone(),
                    registered.raw_output.clone(),
                )?;
                append_operation_diagnostic_log(
                    "debug.read_memory",
                    &session.session_id,
                    &operation_id,
                    &OperationStatus::Failed,
                    registered.artifacts.len(),
                    registered.raw_output.as_ref(),
                    Some(&format!("{code}: {message}")),
                );
                Err(ServiceError::Worker(format!("{code}: {message}")))
            }
            Ok(other) => {
                let message = format!("unexpected read memory response: {other:?}");
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: "debug.read_memory".to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: OperationStatus::Failed,
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    Vec::new(),
                    None,
                )?;
                append_operation_diagnostic_log(
                    "debug.read_memory",
                    &session.session_id,
                    &operation_id,
                    &OperationStatus::Failed,
                    0,
                    None,
                    Some(&message),
                );
                Err(ServiceError::Worker(message))
            }
            Err(error) => {
                let was_canceled =
                    self.operation_status(&operation_id)? == Some(ServiceOperationStatus::Canceled);
                let status = if was_canceled {
                    OperationStatus::Canceled
                } else {
                    OperationStatus::Failed
                };
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "service".to_string(),
                    capability: "debug.read_memory".to_string(),
                    status: status.clone(),
                    created_at: Timestamp::now(),
                    summary: error.to_string(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                workspace.append_command_audit(&CommandAuditRecord {
                    operation_id: operation_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    capability: "debug.read_memory".to_string(),
                    command: audit_command,
                    created_at: Timestamp::now(),
                    status: status.clone(),
                    artifacts: Vec::new(),
                    raw_output: None,
                })?;
                if !was_canceled {
                    self.finish_operation_in_memory(
                        &operation_id,
                        ServiceOperationStatus::Failed,
                        error.to_string(),
                        Vec::new(),
                        None,
                    )?;
                }
                append_operation_diagnostic_log(
                    "debug.read_memory",
                    &session.session_id,
                    &operation_id,
                    &status,
                    0,
                    None,
                    Some(&error.to_string()),
                );
                Err(error)
            }
        }
    }

    pub(super) fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, ServiceState>, ServiceError> {
        self.state
            .lock()
            .map_err(|_| ServiceError::Rpc("service state lock poisoned".to_string()))
    }

    pub(super) fn mark_debug_session_error(
        &self,
        session_id: &SessionRef,
    ) -> Result<(), ServiceError> {
        let mut state = self.lock_state()?;
        if let Some(session) = state.sessions.get_mut(session_id.id.as_str()) {
            session.state = DebugSessionState::Error;
            session.updated_at = Timestamp::now();
        }
        Ok(())
    }

    pub(super) fn operation_status(
        &self,
        operation_id: &OperationRef,
    ) -> Result<Option<ServiceOperationStatus>, ServiceError> {
        let state = self.lock_state()?;
        Ok(state
            .operations
            .get(operation_id.id.as_str())
            .map(|operation| operation.status.clone()))
    }

    pub(super) fn latest_operation_id_from_arguments(&self, arguments: &Value) -> Option<String> {
        let session_id = extract_ref_id_field(arguments, "session_id")?;
        let state = self.state.lock().ok()?;
        state
            .sessions
            .get(&session_id)
            .and_then(|session| session.last_operation.as_ref())
            .map(|operation_id| operation_id.id.as_str().to_string())
    }

    pub(super) fn start_ttd_recording(
        &self,
        recording_id: RecordingRef,
    ) -> Result<TtdActiveGuard, ServiceError> {
        let mut state = self.lock_state()?;
        if let Some(active) = &state.active_ttd_recording {
            return Err(ServiceError::Rpc(format!(
                "another TTD recording is already active: {active}"
            )));
        }
        state.active_ttd_recording = Some(recording_id.clone());
        Ok(TtdActiveGuard {
            state: self.state.clone(),
            recording_id,
        })
    }

    pub(super) fn resolve_ttd_exe(&self) -> Result<PathBuf, ServiceError> {
        let ttd_dir = self
            .capabilities
            .ttd_dir
            .clone()
            .or_else(|| {
                RuntimeConfig::default()
                    .resolve_tool_paths()
                    .ttd
                    .map(|location| location.dir)
            })
            .ok_or_else(|| {
                ServiceError::Rpc(
                    "TTD.exe was not found in the automatic tool search path".to_string(),
                )
            })?;
        let ttd_exe = ttd_dir.join("TTD.exe");
        if !ttd_exe.is_file() {
            return Err(ServiceError::Rpc(format!(
                "TTD.exe was not found under {}",
                ttd_dir.display()
            )));
        }
        Ok(ttd_exe)
    }

    pub(super) fn finish_operation_in_memory(
        &self,
        operation_id: &OperationRef,
        status: ServiceOperationStatus,
        summary: String,
        artifacts: Vec<ArtifactRef>,
        raw_output: Option<ArtifactRef>,
    ) -> Result<(), ServiceError> {
        let mut state = self.lock_state()?;
        if let Some(operation) = state.operations.get_mut(operation_id.id.as_str()) {
            operation.status = status;
            operation.summary = summary;
            operation.artifacts = artifacts;
            operation.raw_output = raw_output;
            operation.updated_at = Timestamp::now();
        }
        Ok(())
    }

    pub(super) fn record_failed_operation_in_memory(
        &self,
        operation_id: &OperationRef,
        capability: &str,
        session_id: Option<SessionRef>,
        summary: String,
        artifacts: Vec<ArtifactRef>,
        raw_output: Option<ArtifactRef>,
    ) -> Result<(), ServiceError> {
        let now = Timestamp::now();
        let mut operation = ServiceOperation::failed(
            operation_id.clone(),
            capability,
            session_id.clone(),
            summary,
        );
        operation.artifacts = artifacts.clone();
        operation.raw_output = raw_output;
        operation.updated_at = now;

        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        if let Some(session_id) = session_id {
            if let Some(session) = state.sessions.get_mut(session_id.id.as_str()) {
                session.updated_at = now;
                session.last_operation = Some(operation_id.clone());
                session.artifacts.extend(artifacts);
            }
        }
        Ok(())
    }

    pub(super) fn record_failed_session_create(
        &self,
        workspace: &Workspace,
        operation_id: &OperationRef,
        session_id: &SessionRef,
        summary: impl Into<String>,
    ) -> Result<(), ServiceError> {
        let summary = summary.into();
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "debug.session.create".to_string(),
            status: OperationStatus::Failed,
            created_at: Timestamp::now(),
            summary: summary.clone(),
            artifacts: Vec::new(),
            raw_output: None,
        })?;

        let mut operation = ServiceOperation::failed(
            operation_id.clone(),
            "debug.session.create",
            Some(session_id.clone()),
            summary,
        );
        operation.updated_at = Timestamp::now();
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        Ok(())
    }

    pub(super) fn reusable_debug_session(
        &self,
        session_id: &SessionRef,
    ) -> Result<ManagedSession, ServiceError> {
        self.reusable_session_with_capability(session_id, "debug")
    }

    pub(super) fn reusable_reverse_session(
        &self,
        session_id: &SessionRef,
    ) -> Result<ManagedSession, ServiceError> {
        self.reusable_session_with_capability(session_id, "reverse")
    }

    pub(super) fn reusable_session_with_capability(
        &self,
        session_id: &SessionRef,
        capability: &str,
    ) -> Result<ManagedSession, ServiceError> {
        let state = self.lock_state()?;
        let session = state
            .sessions
            .get(session_id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::SessionNotFound(session_id.to_string()))?;
        if !session.state.is_reusable() {
            return Err(ServiceError::SessionNotReusable(session_id.to_string()));
        }
        if session.capability != capability {
            return Err(ServiceError::Rpc(format!(
                "session {} is {}, expected {}",
                session_id, session.capability, capability
            )));
        }
        Ok(session)
    }
}
