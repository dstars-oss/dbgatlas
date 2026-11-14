use super::*;

impl ServiceHost {
    pub(super) fn debug_session_create(
        &self,
        params: Option<Value>,
    ) -> Result<Value, ServiceError> {
        let params = parse_debug_session_create_params(params)?;
        let target = params.target.clone().validate()?;
        let request = CreateDebugSession {
            target: target.clone(),
            startup_timeout_ms: params.startup_timeout_ms,
        }
        .validate()?;
        let session_id = next_session_ref();
        let operation_id = next_operation_ref();
        let workspace = ensure_project_workspace(&params.project_root)?;
        let session_dir = workspace.ensure_session_artifact_dir(&session_id.id)?;
        let mut start = self.try_start_debug_worker(
            &params,
            &request,
            &session_id,
            &operation_id,
            &workspace,
            &session_dir,
            params.worker_identity.worker_create_identity(),
            params.worker_identity,
        );
        if start.worker.is_none()
            && should_auto_retry_active_interactive_user(
                params.worker_identity,
                &request.target,
                &start.failures,
            )
        {
            append_service_diagnostic_log(&format!(
                "debug_session_create_auto_retry_active_user session_id={} operation_id={} target_kind={}",
                sanitize_log_value(session_id.id.as_str()),
                sanitize_log_value(operation_id.id.as_str()),
                sanitize_log_value(debug_target_kind(&request.target))
            ));
            let retry = self.try_start_debug_worker(
                &params,
                &request,
                &session_id,
                &operation_id,
                &workspace,
                &session_dir,
                Some(WorkerIdentity::ActiveInteractiveUser),
                DebugWorkerIdentity::ActiveInteractiveUser,
            );
            if retry.worker.is_some() {
                start = retry;
            } else {
                start.failures.extend(retry.failures);
            }
        }
        let Some(worker) = start.worker else {
            let message = start.failures.join(" | ");
            self.record_failed_session_create(&workspace, &operation_id, &session_id, &message)?;
            append_service_diagnostic_log(&format!(
                "debug_session_create_failed session_id={} operation_id={} error={}",
                sanitize_log_value(session_id.id.as_str()),
                sanitize_log_value(operation_id.id.as_str()),
                sanitize_log_value(&message)
            ));
            return Err(ServiceError::Worker(message));
        };
        let start_writes = start
            .writes
            .expect("worker start writes are set with started worker");
        let now = Timestamp::now();
        let registered_start_writes =
            register_worker_writes(&workspace, &operation_id, &start_writes)?;

        let session = ManagedSession {
            session_id: session_id.clone(),
            capability: "debug".to_string(),
            project_root: params.project_root,
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: session_dir,
            target: Some(target),
            state: DebugSessionState::Ready,
            worker,
            request_lock: Arc::new(Mutex::new(())),
            created_at: now,
            updated_at: now,
            last_operation: Some(operation_id.clone()),
            artifacts: registered_start_writes.artifacts.clone(),
        };

        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "debug.session.create",
            Some(session_id.clone()),
            "debug session created",
        );
        operation.artifacts = registered_start_writes.artifacts.clone();
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "debug.session.create".to_string(),
            status: OperationStatus::Success,
            created_at: now,
            summary: "debug session created".to_string(),
            artifacts: registered_start_writes.artifacts.clone(),
            raw_output: registered_start_writes.raw_output.clone(),
        })?;

        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        state
            .sessions
            .insert(session_id.id.as_str().to_string(), session.clone());

        append_service_diagnostic_log(&format!(
            "debug_session_create_complete session_id={} operation_id={} artifact_count={}",
            sanitize_log_value(session_id.id.as_str()),
            sanitize_log_value(operation_id.id.as_str()),
            registered_start_writes.artifacts.len()
        ));

        Ok(json!({
            "session_id": session_id,
            "state": session.state,
            "operation_id": operation_id,
            "operation_status": "success",
            "artifact_refs": registered_start_writes.artifacts,
            "raw_output_ref": registered_start_writes.raw_output,
            "operation": {
                "status": "success",
                "artifact_refs": registered_start_writes.artifacts,
                "raw_output_ref": registered_start_writes.raw_output,
            },
        }))
    }

    pub(super) fn debug_session_close(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_session(params, SessionFinishMode::Close)
    }

    pub(super) fn debug_session_kill(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        self.finish_session(params, SessionFinishMode::Kill)
    }

    pub(super) fn finish_session(
        &self,
        params: Option<Value>,
        mode: SessionFinishMode,
    ) -> Result<Value, ServiceError> {
        let params: SessionParams = parse_params(params)?;
        let mut session = {
            let state = self.lock_state()?;
            state
                .sessions
                .get(params.session_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(params.session_id.to_string()))?
        };
        if session.state == DebugSessionState::Closed {
            return Err(ServiceError::SessionAlreadyTerminal(
                params.session_id.to_string(),
            ));
        }
        if session.capability != "debug" {
            return Err(ServiceError::Rpc(format!(
                "session {} is {}, expected debug",
                params.session_id, session.capability
            )));
        }
        let request_lock = session.request_lock.clone();
        let _request_guard = match mode {
            SessionFinishMode::Close => Some(
                request_lock
                    .lock()
                    .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?,
            ),
            SessionFinishMode::Kill => None,
        };
        session = {
            let state = self.lock_state()?;
            state
                .sessions
                .get(params.session_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(params.session_id.to_string()))?
        };
        if session.state == DebugSessionState::Closed {
            return Err(ServiceError::SessionAlreadyTerminal(
                params.session_id.to_string(),
            ));
        }
        if session.capability != "debug" {
            return Err(ServiceError::Rpc(format!(
                "session {} is {}, expected debug",
                params.session_id, session.capability
            )));
        }
        let operation_id = next_operation_ref();
        if session.state != DebugSessionState::Error {
            match mode {
                SessionFinishMode::Close => {
                    let close_response = self.supervisor.request_worker(
                        &session.worker,
                        WorkerRequest::CloseSession {
                            session_id: session.session_id.clone(),
                        },
                    );
                    match close_response {
                        Err(error) => {
                            let cleanup = self.supervisor.kill_worker(&session.worker);
                            self.mark_debug_session_error(&session.session_id)?;
                            let cleanup_message = match cleanup {
                                Ok(()) => "worker was killed after close failure".to_string(),
                                Err(cleanup_error) => format!(
                                    "worker cleanup also failed: {cleanup_error}; use debug.session.kill"
                                ),
                            };
                            return Err(ServiceError::Worker(format!(
                                "debug.session.close could not complete cooperatively: {error}; {cleanup_message}"
                            )));
                        }
                        Ok(WorkerResponse::Ok { .. }) => {
                            self.supervisor.close_worker(&session.worker)?
                        }
                        Ok(WorkerResponse::Failed { code, message, .. }) => {
                            return Err(ServiceError::Worker(format!(
                                "{code}: {message}; use debug.session.kill if the worker is stuck"
                            )));
                        }
                        Ok(other) => {
                            return Err(ServiceError::Worker(format!(
                                "unexpected close response: {other:?}; use debug.session.kill if the worker is stuck"
                            )));
                        }
                    }
                }
                SessionFinishMode::Kill => self.supervisor.kill_worker(&session.worker)?,
            }
        }
        session.state = DebugSessionState::Closed;
        session.updated_at = Timestamp::now();
        session.last_operation = Some(operation_id.clone());
        {
            let mut state = self.lock_state()?;
            state.sessions.remove(session.session_id.id.as_str());
        }
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let capability = match mode {
            SessionFinishMode::Close => "debug.session.close",
            SessionFinishMode::Kill => "debug.session.kill",
        };
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: capability.to_string(),
            status: OperationStatus::Success,
            created_at: Timestamp::now(),
            summary: format!("{capability} complete"),
            artifacts: Vec::new(),
            raw_output: None,
        })?;

        let operation = ServiceOperation::success(
            operation_id.clone(),
            capability,
            Some(session.session_id.clone()),
            format!("{capability} complete"),
        );
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        Ok(json!({
            "session_id": session.session_id,
            "state": session.state,
            "operation_id": operation_id,
            "operation_status": "success",
            "artifact_refs": [],
            "raw_output_ref": null,
            "operation": {
                "status": "success",
                "artifact_refs": [],
                "raw_output_ref": null,
            },
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_start_debug_worker(
        &self,
        params: &DebugSessionCreateParams,
        request: &CreateDebugSession,
        session_id: &SessionRef,
        operation_id: &OperationRef,
        workspace: &Workspace,
        session_dir: &Path,
        worker_identity: Option<WorkerIdentity>,
        failure_identity: DebugWorkerIdentity,
    ) -> DebugSessionStartAttempt {
        let mut started_worker = None;
        let mut start_writes = None;
        let mut start_failures = Vec::new();
        let dbgeng_attempts =
            debug_worker_dbgeng_attempts(&request.target, &self.capabilities.dbgeng_dirs);
        append_service_diagnostic_log(&format!(
            "debug_session_create_start session_id={} operation_id={} target_kind={} identity={} attempt_count={} workspace={}",
            sanitize_log_value(session_id.id.as_str()),
            sanitize_log_value(operation_id.id.as_str()),
            sanitize_log_value(debug_target_kind(&request.target)),
            sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
            dbgeng_attempts.len(),
            sanitize_log_value(&workspace.root().display().to_string())
        ));
        for (attempt_index, dbgeng_dirs) in dbgeng_attempts.into_iter().enumerate() {
            append_service_diagnostic_log(&format!(
                "debug_session_create_attempt session_id={} operation_id={} identity={} attempt={} dbgeng_dir_count={}",
                sanitize_log_value(session_id.id.as_str()),
                sanitize_log_value(operation_id.id.as_str()),
                sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                attempt_index + 1,
                dbgeng_dirs.len()
            ));
            let worker = match self.supervisor.create_worker(WorkerCreateRequest {
                session_id: session_id.clone(),
                project_root: params.project_root.clone(),
                internal_workspace_root: workspace.root().to_path_buf(),
                artifact_dir: session_dir.to_path_buf(),
                startup_timeout_ms: request
                    .startup_timeout_ms
                    .unwrap_or(DEFAULT_DEBUG_SESSION_STARTUP_TIMEOUT_MS),
                dbgeng_dirs,
                identity: worker_identity.clone(),
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    let message = add_debug_access_denied_hint(error.to_string(), failure_identity);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_error session_id={} operation_id={} identity={} attempt={} error={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1,
                        sanitize_log_value(&message)
                    ));
                    start_failures.push(message);
                    break;
                }
            };
            let start = self.supervisor.request_worker(
                &worker,
                WorkerRequest::StartDebugSession {
                    session_id: session_id.clone(),
                    target: request.target.clone(),
                    artifact_dir: session_dir.to_path_buf(),
                },
            );
            match start {
                Ok(WorkerResponse::Ok { writes, .. }) => {
                    started_worker = Some(worker);
                    start_writes = Some(writes);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_ok session_id={} operation_id={} identity={} attempt={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1
                    ));
                    break;
                }
                Ok(WorkerResponse::Failed { code, message, .. }) => {
                    let _ = self.supervisor.kill_worker(&worker);
                    let failure = debug_start_failure_message(code, message, failure_identity);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_failed session_id={} operation_id={} identity={} attempt={} code={} error={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1,
                        sanitize_log_value(failure.code.as_str()),
                        sanitize_log_value(failure.message.as_str())
                    ));
                    start_failures.push(failure.to_string());
                }
                Ok(other) => {
                    let _ = self.supervisor.kill_worker(&worker);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_unexpected session_id={} operation_id={} identity={} attempt={} response={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1,
                        sanitize_log_value(&format!("{other:?}"))
                    ));
                    start_failures.push(format!("unexpected start response: {other:?}"));
                    break;
                }
                Err(error) => {
                    let _ = self.supervisor.kill_worker(&worker);
                    let message = add_debug_access_denied_hint(error.to_string(), failure_identity);
                    append_service_diagnostic_log(&format!(
                        "debug_session_create_attempt_error session_id={} operation_id={} identity={} attempt={} error={}",
                        sanitize_log_value(session_id.id.as_str()),
                        sanitize_log_value(operation_id.id.as_str()),
                        sanitize_log_value(debug_worker_identity_label(worker_identity.as_ref())),
                        attempt_index + 1,
                        sanitize_log_value(&message)
                    ));
                    start_failures.push(message);
                    break;
                }
            }
        }

        DebugSessionStartAttempt {
            worker: started_worker,
            writes: start_writes,
            failures: start_failures,
        }
    }

    pub(super) fn debug_eval(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugEvalParams = parse_params(params)?;
        validate_optional_timeout_ms(params.timeout_ms)?;
        let request = EvalDebugCommand {
            session_id: params.session_id.clone(),
            command: params.command,
            timeout_ms: params.timeout_ms,
        };
        request.validate()?;
        self.eval_command(request, "debug.eval")
    }

    pub(super) fn debug_eval_steps(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugEvalStepsParams = parse_params(params)?;
        validate_optional_timeout_ms(params.timeout_ms)?;
        if params.commands.is_empty() {
            return Err(ServiceError::Rpc(
                "debug.eval_steps commands must not be empty".to_string(),
            ));
        }
        for command in &params.commands {
            EvalDebugCommand {
                session_id: params.session_id.clone(),
                command: command.clone(),
                timeout_ms: params.timeout_ms,
            }
            .validate()?;
        }

        let session = self.reusable_debug_session(&params.session_id)?;
        let request_lock = session.request_lock.clone();
        let _request_guard = request_lock
            .lock()
            .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
        let session = self.reusable_debug_session(&params.session_id)?;
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let batch_operation_id = next_operation_ref();
        let now = Timestamp::now();
        {
            let mut state = self.lock_state()?;
            state.operations.insert(
                batch_operation_id.id.as_str().to_string(),
                ServiceOperation::running(
                    batch_operation_id.clone(),
                    "debug.eval_steps",
                    Some(session.session_id.clone()),
                    "debug.eval_steps running",
                ),
            );
        }

        let mut steps = Vec::new();
        let mut artifact_refs = Vec::new();
        let mut raw_output_ref = None;
        let mut failed_step_index = None;
        let mut canceled_step_index = None;
        for (index, command) in params.commands.iter().enumerate() {
            let step_operation_id = next_operation_ref();
            {
                let mut state = self.lock_state()?;
                state.operations.insert(
                    step_operation_id.id.as_str().to_string(),
                    ServiceOperation::running(
                        step_operation_id.clone(),
                        "debug.eval_steps.step",
                        Some(session.session_id.clone()),
                        "debug.eval_steps step running",
                    ),
                );
            }

            let worker_response = self.request_worker_with_optional_timeout(
                &session,
                WorkerRequest::EvalDebugCommand {
                    session_id: session.session_id.clone(),
                    operation_id: step_operation_id.clone(),
                    command: command.clone(),
                    artifact_dir: session.artifact_dir.clone(),
                },
                &step_operation_id,
                params.timeout_ms,
            );
            let step_result = self.finish_command_worker_response(
                &session,
                &workspace,
                step_operation_id.clone(),
                "debug.eval_steps.step",
                command.clone(),
                worker_response,
            );
            match step_result {
                Ok(mut value) => {
                    if let Some(object) = value.as_object_mut() {
                        object.insert("step_index".to_string(), json!(index));
                    }
                    let state = self.lock_state()?;
                    if let Some(operation) = state.operations.get(step_operation_id.id.as_str()) {
                        if operation.status == ServiceOperationStatus::Canceled
                            && canceled_step_index.is_none()
                        {
                            canceled_step_index = Some(index);
                        }
                        artifact_refs.extend(operation.artifacts.clone());
                        raw_output_ref = operation.raw_output.clone().or(raw_output_ref);
                    }
                    steps.push(value);
                }
                Err(error) => {
                    let state = self.lock_state()?;
                    let (step_status, step_artifacts, step_raw_output) = state
                        .operations
                        .get(step_operation_id.id.as_str())
                        .map(|operation| {
                            (
                                operation.status.clone(),
                                operation.artifacts.clone(),
                                operation.raw_output.clone(),
                            )
                        })
                        .unwrap_or((ServiceOperationStatus::Running, Vec::new(), None));
                    drop(state);
                    if step_status == ServiceOperationStatus::Running {
                        self.finish_operation_in_memory(
                            &step_operation_id,
                            ServiceOperationStatus::Failed,
                            error.to_string(),
                            Vec::new(),
                            None,
                        )?;
                        self.finish_operation_in_memory(
                            &batch_operation_id,
                            ServiceOperationStatus::Failed,
                            error.to_string(),
                            artifact_refs.clone(),
                            raw_output_ref.clone(),
                        )?;
                        return Err(error);
                    }
                    artifact_refs.extend(step_artifacts);
                    raw_output_ref = step_raw_output.or(raw_output_ref);
                    let operation_status = match step_status {
                        ServiceOperationStatus::Canceled => {
                            if canceled_step_index.is_none() {
                                canceled_step_index = Some(index);
                            }
                            "canceled"
                        }
                        _ => {
                            if failed_step_index.is_none() {
                                failed_step_index = Some(index);
                            }
                            "failed"
                        }
                    };
                    if operation_status == "canceled" && !params.continue_on_error {
                        steps.push(json!({
                            "step_index": index,
                            "session_id": session.session_id,
                            "operation_id": step_operation_id,
                            "operation_status": operation_status,
                            "command": command,
                            "error": error.to_string(),
                        }));
                        break;
                    }
                    steps.push(json!({
                        "step_index": index,
                        "session_id": session.session_id,
                        "operation_id": step_operation_id,
                        "operation_status": operation_status,
                        "command": command,
                        "error": error.to_string(),
                    }));
                    if !params.continue_on_error {
                        break;
                    }
                }
            }

            let current_session = self.reusable_debug_session(&session.session_id);
            if current_session.is_err() {
                if failed_step_index.is_none() {
                    failed_step_index = Some(index);
                }
                break;
            }
        }

        let batch_was_canceled =
            self.operation_status(&batch_operation_id)? == Some(ServiceOperationStatus::Canceled);
        let status = if batch_was_canceled || canceled_step_index.is_some() {
            OperationStatus::Canceled
        } else if failed_step_index.is_some() {
            OperationStatus::Failed
        } else {
            OperationStatus::Success
        };
        let summary = match (canceled_step_index, failed_step_index) {
            (Some(index), _) => format!("debug.eval_steps canceled at step {index}"),
            (_, Some(index)) => format!("debug.eval_steps failed at step {index}"),
            _ if batch_was_canceled => "debug.eval_steps canceled".to_string(),
            _ => "debug.eval_steps completed".to_string(),
        };
        workspace.append_operation(&OperationRecord {
            operation_id: batch_operation_id.clone(),
            adapter_id: "service".to_string(),
            capability: "debug.eval_steps".to_string(),
            status: status.clone(),
            created_at: now,
            summary: summary.clone(),
            artifacts: artifact_refs.clone(),
            raw_output: raw_output_ref.clone(),
        })?;
        workspace.append_command_audit(&CommandAuditRecord {
            operation_id: batch_operation_id.clone(),
            session_id: Some(session.session_id.clone()),
            capability: "debug.eval_steps".to_string(),
            command: params.commands.join("\n"),
            created_at: now,
            status: status.clone(),
            artifacts: artifact_refs.clone(),
            raw_output: raw_output_ref.clone(),
        })?;

        let service_status = match status {
            OperationStatus::Failed => ServiceOperationStatus::Failed,
            OperationStatus::Canceled => ServiceOperationStatus::Canceled,
            _ => ServiceOperationStatus::Success,
        };
        self.finish_operation_in_memory(
            &batch_operation_id,
            service_status,
            summary,
            artifact_refs.clone(),
            raw_output_ref.clone(),
        )?;
        {
            let mut state = self.lock_state()?;
            if let Some(session_state) = state.sessions.get_mut(session.session_id.id.as_str()) {
                session_state.last_operation = Some(batch_operation_id.clone());
                session_state.updated_at = Timestamp::now();
            }
        }

        Ok(json!({
            "session_id": session.session_id,
            "operation_id": batch_operation_id,
            "operation_status": status,
            "failed_step_index": failed_step_index,
            "steps": steps,
            "artifact_refs": artifact_refs,
            "raw_output_ref": raw_output_ref,
            "operation": {
                "status": status,
                "artifact_refs": artifact_refs,
                "raw_output_ref": raw_output_ref,
            },
        }))
    }

    pub(super) fn debug_builtin_eval(
        &self,
        params: Option<Value>,
        capability: &'static str,
        command: &'static str,
    ) -> Result<Value, ServiceError> {
        let params: SessionParams = parse_params(params)?;
        let request = EvalDebugCommand {
            session_id: params.session_id,
            command: command.to_string(),
            timeout_ms: None,
        };
        self.eval_command(request, capability)
    }

    pub(super) fn debug_add_symbols(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugAddSymbolsParams = parse_params(params)?;
        let request = AddSymbolsRequest {
            session_id: params.session_id,
            symbol_path: params.symbol_path,
            reload: params.reload,
        };
        request.validate()?;
        self.add_symbols(request)
    }

    pub(super) fn debug_read_memory(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: DebugReadMemoryParams = parse_params(params)?;
        let request = ReadMemoryRequest {
            session_id: params.session_id,
            address: parse_u64_param(&params.address, "address")?,
            length: params.length,
        };
        request.validate(MAX_MEMORY_READ_LENGTH)?;
        self.read_memory(request)
    }
}
