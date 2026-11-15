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
        let session_id = self
            .execution_scope
            .as_ref()
            .map(|s| s.session_id.clone())
            .unwrap_or_else(next_session_ref);
        let operation_id = self.action_operation_id();
        let workspace = ensure_project_workspace(&params.project_root)?;
        let session_dir = workspace.ensure_session_artifact_dir(&session_id.id)?;
        if self.execution_scope.is_none() {
            self.open_session_record(
                &session_id,
                &workspace,
                dbgatlas_model::SessionKind::Debug,
                serde_json::to_value(&target)?,
                &operation_id,
                "debug.session.create",
                serde_json::to_value(&request)?,
            )?;
        }
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
        if !matches!(request.target, DebugTarget::Launch { .. })
            && start.worker.is_none()
            && !start.failures.iter().any(|f| f.contains("cleanup"))
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
                start.uncleaned_worker = retry.uncleaned_worker;
            }
        }
        let Some(worker) = start.worker else {
            let message = start.failures.join(" | ");
            let cleanup_complete = start.uncleaned_worker.is_none();
            if let Some(worker) = start.uncleaned_worker {
                let now = Timestamp::now();
                self.lock_state()?.sessions.insert(
                    session_id.to_string(),
                    ManagedSession {
                        session_id: session_id.clone(),
                        capability: "debug".into(),
                        project_root: params.project_root.clone(),
                        internal_workspace_root: workspace.root().into(),
                        artifact_dir: session_dir.clone(),
                        target: Some(target.clone()),
                        state: DebugSessionState::Error,
                        worker: worker.clone(),
                        request_lock: Arc::new(Mutex::new(())),
                        created_at: now,
                        updated_at: now,
                        last_operation: Some(operation_id.clone()),
                        artifacts: vec![],
                    },
                );
                self.bind_session_worker(&session_id, &worker, "dbgeng")?;
            }
            self.set_session_terminal(&session_id, true, cleanup_complete, Some(message.clone()))?;
            self.record_failed_session_create(&workspace, &operation_id, &session_id, &message)?;
            append_service_diagnostic_log(&format!(
                "debug_session_create_failed session_id={} operation_id={} error={}",
                sanitize_log_value(session_id.id.as_str()),
                sanitize_log_value(operation_id.id.as_str()),
                sanitize_log_value(&message)
            ));
            return Err(ServiceError::Worker(format!(
                "{message}; session_id={session_id}; operation_id={operation_id}"
            )));
        };
        let start_writes = start
            .writes
            .expect("worker start writes are set with started worker");
        let now = Timestamp::now();
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
            artifacts: vec![],
        };

        self.lock_state()?
            .sessions
            .insert(session_id.to_string(), session.clone());
        self.bind_session_worker(&session_id, &session.worker, "dbgeng")?;
        let registered_start_writes = match register_worker_writes(
            &workspace,
            &operation_id,
            &start_writes,
        ) {
            Ok(value) => value,
            Err(error) => {
                self.record_debug_artifact_registration_failure(
                    &session,
                    &workspace,
                    &operation_id,
                    "debug.session.create",
                    "create",
                    OperationStatus::Success,
                    &error,
                )?;
                return Ok(
                    json!({"session_id":session_id,"operation_id":operation_id,"operation_status":"success",
                    "output_error":error.to_string(),"artifact_refs":[],"state":"ready"}),
                );
            }
        };
        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "debug.session.create",
            Some(session_id.clone()),
            "debug session created",
        );
        operation.artifacts = registered_start_writes.artifacts.clone();
        self.append_operation_result(
            &workspace,
            &OperationRecord {
                details: Default::default(),
                operation_id: operation_id.clone(),
                adapter_id: "service".to_string(),
                capability: "debug.session.create".to_string(),
                status: OperationStatus::Success,
                created_at: now,
                summary: "debug session created".to_string(),
                artifacts: registered_start_writes.artifacts.clone(),
                raw_output: registered_start_writes.raw_output.clone(),
            },
        )?;

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

    pub(super) fn session_interrupt(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        #[derive(Deserialize)]
        struct Params {
            session_id: SessionRef,
            #[serde(alias = "expected_run_epoch")]
            expected_epoch: Option<u64>,
        }
        let params: Params = parse_params(params)?;
        let session = self.reusable_debug_session(&params.session_id)?;
        let observation = self
            .supervisor
            .snapshot(&session.worker)
            .ok_or_else(|| ServiceError::Worker("no backend observation available".into()))?;
        if observation.context["target_control"] == false
            && observation.target_state != dbgatlas_model::TargetState::Offline
        {
            return Ok(
                json!({"accepted":false,"code":"unsupported","session_id":params.session_id}),
            );
        }
        if observation.context["engine"]["state"] == "idle" && params.expected_epoch.is_none() {
            return Ok(
                json!({"accepted":false,"code":"already_stopped","session_id":params.session_id}),
            );
        }
        let epoch = params
            .expected_epoch
            .or_else(|| observation.context["engine"]["epoch"].as_u64())
            .ok_or_else(|| {
                ServiceError::Worker(
                    "no active target run; use operation.cancel for an active command".into(),
                )
            })?;
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let id = self.action_operation_id();
        self.begin_operation_record(
            &workspace,
            &id,
            &session.session_id,
            "session.interrupt",
            json!({"expected_epoch":epoch}),
        )?;
        let delivered = self.supervisor.interrupt_worker(&session.worker, epoch);
        let accepted = delivered.as_ref().is_ok_and(|result| result.accepted);
        let summary = match &delivered {
            Ok(result) => result.code.clone(),
            Err(error) => error.to_string(),
        };
        self.append_operation_result(
            &workspace,
            &OperationRecord {
                details: Default::default(),
                operation_id: id.clone(),
                adapter_id: "service".into(),
                capability: "session.interrupt".into(),
                status: if accepted {
                    OperationStatus::Success
                } else {
                    OperationStatus::Failed
                },
                created_at: Timestamp::now(),
                summary: summary.clone(),
                artifacts: vec![],
                raw_output: None,
            },
        )?;
        self.finish_operation_in_memory(
            &id,
            if accepted {
                ServiceOperationStatus::Success
            } else {
                ServiceOperationStatus::Failed
            },
            summary,
            vec![],
            None,
        )?;
        let result = delivered?;
        Ok(
            json!({"execution_id":id,"session_id":session.session_id,"accepted":result.accepted,"epoch":result.epoch,"native_hresult":result.native_hresult,"code":result.code,"session":self.session_get(Some(json!({"session_id":session.session_id})))?}),
        )
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
        let session = self
            .lock_state()?
            .sessions
            .get(params.session_id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::SessionNotFound(params.session_id.to_string()))?;
        let _guard = if matches!(mode, SessionFinishMode::Close) {
            Some(
                session
                    .request_lock
                    .try_lock()
                    .map_err(|_| ServiceError::SessionBusy(params.session_id.to_string()))?,
            )
        } else {
            None
        };
        if !self
            .lock_state()?
            .sessions
            .contains_key(params.session_id.id.as_str())
        {
            return Err(ServiceError::SessionNotFound(params.session_id.to_string()));
        }
        let operation_id = self.action_operation_id();
        let capability = if matches!(mode, SessionFinishMode::Close) {
            "debug.session.close"
        } else {
            "debug.session.kill"
        };
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        self.begin_operation_record(
            &workspace,
            &operation_id,
            &session.session_id,
            capability,
            json!({"session_id":session.session_id}),
        )?;
        if let Some(summary) = self
            .lock_state()?
            .summaries
            .get_mut(session.session_id.id.as_str())
        {
            summary.lifecycle = dbgatlas_model::SessionLifecycle::Closing;
            self.persist_session(summary)?;
        }
        let result = if matches!(mode, SessionFinishMode::Kill) {
            self.supervisor.kill_worker(&session.worker)
        } else {
            match self.supervisor.request_worker(
                &session.worker,
                WorkerRequest::CloseSession {
                    session_id: session.session_id.clone(),
                },
            ) {
                Ok(WorkerResponse::Ok { .. }) => self.supervisor.close_worker(&session.worker),
                Ok(other) => Err(ServiceError::Worker(format!(
                    "cooperative close failed: {other:?}; explicit force close is available"
                ))),
                Err(error) => Err(error),
            }
        };
        let complete = result.is_ok();
        let message = result.as_ref().err().map(ToString::to_string);
        self.set_session_terminal(
            &session.session_id,
            !complete || matches!(mode, SessionFinishMode::Kill),
            complete,
            message.clone(),
        )?;
        if complete {
            self.lock_state()?
                .sessions
                .remove(session.session_id.id.as_str());
        } else if let Some(current) = self
            .lock_state()?
            .sessions
            .get_mut(session.session_id.id.as_str())
        {
            current.state = DebugSessionState::Error;
        }
        let status = if complete {
            OperationStatus::Success
        } else {
            OperationStatus::Failed
        };
        self.finish_operation_in_memory(
            &operation_id,
            status.clone(),
            message
                .clone()
                .unwrap_or_else(|| format!("{capability} complete")),
            vec![],
            None,
        )?;
        self.append_operation_result(
            &workspace,
            &OperationRecord {
                details: Default::default(),
                operation_id: operation_id.clone(),
                adapter_id: "service".into(),
                capability: capability.into(),
                status,
                created_at: Timestamp::now(),
                summary: message.unwrap_or_else(|| format!("{capability} complete")),
                artifacts: vec![],
                raw_output: None,
            },
        )?;
        result?;
        Ok(
            json!({"session_id":session.session_id,"state":DebugSessionState::Closed,"operation_id":operation_id,
            "operation_status":"success","artifact_refs":[],"raw_output_ref":null,
            "operation":{"status":"success","artifact_refs":[],"raw_output_ref":null}}),
        )
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
        let mut result = DebugSessionStartAttempt {
            worker: None,
            writes: None,
            failures: vec![],
            uncleaned_worker: None,
        };
        for dbgeng_dirs in
            debug_worker_dbgeng_attempts(&request.target, &self.capabilities.dbgeng_dirs)
        {
            append_service_diagnostic_log(&format!(
                "debug_open_candidate session_id={session_id} operation_id={operation_id} identity={}",
                debug_worker_identity_label(worker_identity.as_ref())
            ));
            let worker = match self.supervisor.create_worker(WorkerCreateRequest {
                session_id: session_id.clone(),
                project_root: params.project_root.clone(),
                internal_workspace_root: workspace.root().into(),
                artifact_dir: session_dir.into(),
                startup_timeout_ms: request
                    .startup_timeout_ms
                    .unwrap_or(DEFAULT_DEBUG_SESSION_STARTUP_TIMEOUT_MS),
                dbgeng_dirs,
                identity: worker_identity.clone(),
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    result.failures.push(add_debug_access_denied_hint(
                        error.to_string(),
                        failure_identity,
                    ));
                    break;
                }
            };
            let response = self
                .start_open_operation(workspace, operation_id, session_id, &worker, "dbgeng")
                .and_then(|()| {
                    self.supervisor.request_worker(
                        &worker,
                        WorkerRequest::StartDebugSession {
                            session_id: session_id.clone(),
                            target: request.target.clone(),
                            artifact_dir: session_dir.into(),
                        },
                    )
                });
            let retry_safe = matches!(response, Ok(WorkerResponse::Failed { .. }))
                && !matches!(request.target, DebugTarget::Launch { .. });
            match response {
                Ok(WorkerResponse::Ok { writes, .. }) => {
                    result.worker = Some(worker);
                    result.writes = Some(writes);
                    break;
                }
                Ok(WorkerResponse::Failed { code, message, .. }) => result
                    .failures
                    .push(debug_start_failure_message(code, message, failure_identity).to_string()),
                Ok(other) => result
                    .failures
                    .push(format!("unexpected opening response: {other:?}")),
                Err(error) => result.failures.push(add_debug_access_denied_hint(
                    error.to_string(),
                    failure_identity,
                )),
            }
            match self.supervisor.kill_worker(&worker) {
                Ok(()) => {}
                Err(error) => {
                    result.failures.push(format!(
                        "candidate cleanup failed: {error}; retry prohibited"
                    ));
                    result.uncleaned_worker = Some(worker);
                    break;
                }
            }
            if !retry_safe {
                break;
            }
        }
        result
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
            .try_lock()
            .map_err(|_| ServiceError::SessionBusy(params.session_id.to_string()))?;
        let session = self.reusable_debug_session(&params.session_id)?;
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let batch_operation_id = self.action_operation_id();
        let now = Timestamp::now();
        self.begin_operation_record(&workspace, &batch_operation_id, &session.session_id,
            "debug.eval_steps", json!({"commands":params.commands,"continue_on_error":params.continue_on_error,"timeout_ms":params.timeout_ms}))?;
        let mut unknown_step = false;
        let mut output_error = None;
        let mut steps = Vec::new();
        let mut artifact_refs = Vec::new();
        let mut raw_output_ref = None;
        let mut failed_step_index = None;
        let mut canceled_step_index = None;
        let mut stop_reason: Option<&str> = None;
        for (index, command) in params.commands.iter().enumerate() {
            if self.batch_stop_requested(&batch_operation_id, &session.session_id) {
                stop_reason = Some("control_requested");
                break;
            }
            if self
                .supervisor
                .snapshot(&session.worker)
                .is_some_and(|s| s.context["engine"]["state"] == "waiting_for_event")
            {
                stop_reason = Some("target_running");
                break;
            }
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

            self.begin_operation_record(&workspace, &step_operation_id, &session.session_id,
                "debug.eval_steps.step", json!({"command":command,"batch_operation_id":batch_operation_id,"step_index":index}))?;
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
                    unknown_step |= step_status == ServiceOperationStatus::Unknown;
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
                        ServiceOperationStatus::Success => {
                            output_error = Some(error.to_string());
                            "success"
                        }
                        ServiceOperationStatus::Unknown => {
                            if failed_step_index.is_none() {
                                failed_step_index = Some(index);
                            }
                            "unknown"
                        }
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
                    if matches!(operation_status, "canceled" | "unknown") {
                        stop_reason = Some(if operation_status == "canceled" {
                            "canceled"
                        } else {
                            "unknown"
                        });
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

            if canceled_step_index.is_some() {
                stop_reason = Some("canceled");
                break;
            }
            if canceled_step_index.is_some() {
                stop_reason = Some("canceled");
                break;
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
        let status = if batch_was_canceled
            || canceled_step_index.is_some()
            || stop_reason == Some("control_requested")
        {
            OperationStatus::Canceled
        } else if unknown_step {
            OperationStatus::Unknown
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
        self.append_operation_result(
            &workspace,
            &OperationRecord {
                details: dbgatlas_model::OperationDetails {
                    output_error: output_error.clone(),
                    ..Default::default()
                },
                operation_id: batch_operation_id.clone(),
                adapter_id: "service".to_string(),
                capability: "debug.eval_steps".to_string(),
                status: status.clone(),
                created_at: now,
                summary: summary.clone(),
                artifacts: artifact_refs.clone(),
                raw_output: raw_output_ref.clone(),
            },
        )?;
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

        let service_status = status.clone();
        self.finish_operation_in_memory(
            &batch_operation_id,
            service_status,
            summary,
            artifact_refs.clone(),
            raw_output_ref.clone(),
        )?;
        {
            let mut state = self.lock_state()?;
            if let Some(op) = state.operations.get_mut(batch_operation_id.id.as_str()) {
                op.details.output_error = output_error.clone();
            }
            if let Some(summary) = state.summaries.get_mut(session.session_id.id.as_str()) {
                summary.last_operation_id = Some(batch_operation_id.clone());
            }
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
            "stop_reason": stop_reason,
            "unexecuted_steps": (steps.len()..params.commands.len()).collect::<Vec<_>>(),
            "stop_reason": stop_reason,
            "unexecuted_steps": (steps.len()..params.commands.len()).collect::<Vec<_>>(),
            "output_error": output_error,
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
