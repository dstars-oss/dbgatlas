use super::*;

impl ServiceHost {
    pub fn new(supervisor: Arc<dyn WorkerSupervisor>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ServiceState::default())),
            supervisor,
            capabilities: ServiceCapabilities::from_runtime_config(&RuntimeConfig::default()),
            ttd_runner: Arc::new(ProcessTtdRecorderRunner),
        }
    }

    pub fn with_mock_workers() -> Self {
        Self::new(Arc::new(MockWorkerSupervisor::new()))
    }

    pub fn with_process_workers() -> Result<Self, ServiceError> {
        Ok(Self::new(Arc::new(ProcessWorkerSupervisor::new()?)))
    }

    pub fn with_process_worker_exe(worker_exe: PathBuf) -> Result<Self, ServiceError> {
        Ok(Self::new(Arc::new(
            ProcessWorkerSupervisor::new_with_worker_exe(worker_exe)?,
        )))
    }

    pub fn with_installed_process_workers() -> Result<Self, ServiceError> {
        Ok(Self::new(Arc::new(
            ProcessWorkerSupervisor::new_installed_service()?,
        )))
    }

    pub fn with_capabilities(mut self, capabilities: ServiceCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    #[cfg(test)]
    pub(super) fn with_ttd_runner(mut self, runner: Arc<dyn TtdRecorderRunner>) -> Self {
        self.ttd_runner = runner;
        self
    }

    pub(super) fn shutdown_workers_gracefully(&self, timeout: Duration) {
        let sessions = match self.lock_state() {
            Ok(state) => state
                .sessions
                .values()
                .filter(|session| !session.state.is_terminal())
                .cloned()
                .collect::<Vec<_>>(),
            Err(error) => {
                append_service_diagnostic_log(&format!(
                    "worker_shutdown_snapshot_failed error={}",
                    sanitize_log_value(&error.to_string())
                ));
                return;
            }
        };
        append_service_diagnostic_log(&format!(
            "worker_shutdown_start session_count={} timeout_ms={}",
            sessions.len(),
            timeout.as_millis()
        ));
        for session in sessions {
            self.shutdown_session_worker(session, timeout);
        }
        append_service_diagnostic_log("worker_shutdown_complete");
    }

    pub(super) fn shutdown_session_worker(&self, session: ManagedSession, timeout: Duration) {
        let started = Instant::now();
        loop {
            match session.request_lock.try_lock() {
                Ok(_guard) => {
                    let request = match session.capability.as_str() {
                        "reverse" => WorkerRequest::CloseReverseSession {
                            session_id: session.session_id.clone(),
                        },
                        "debug" => WorkerRequest::CloseSession {
                            session_id: session.session_id.clone(),
                        },
                        _ => {
                            let _ = self.supervisor.kill_worker(&session.worker);
                            return;
                        }
                    };
                    match self.supervisor.request_worker_with_timeout(
                        &session.worker,
                        request,
                        timeout,
                    ) {
                        Ok(WorkerResponse::Ok { .. }) => {
                            if let Err(error) = self.supervisor.close_worker(&session.worker) {
                                append_service_diagnostic_log(&format!(
                                    "worker_shutdown_close_failed worker_id={} session_id={} capability={} error={}",
                                    sanitize_log_value(session.worker.worker_id.as_str()),
                                    sanitize_log_value(session.session_id.id.as_str()),
                                    sanitize_log_value(&session.capability),
                                    sanitize_log_value(&error.to_string())
                                ));
                                let _ = self.supervisor.kill_worker(&session.worker);
                            }
                        }
                        Ok(other) => {
                            append_service_diagnostic_log(&format!(
                                "worker_shutdown_unexpected_response worker_id={} session_id={} capability={} response={}",
                                sanitize_log_value(session.worker.worker_id.as_str()),
                                sanitize_log_value(session.session_id.id.as_str()),
                                sanitize_log_value(&session.capability),
                                sanitize_log_value(&format!("{other:?}"))
                            ));
                            let _ = self.supervisor.kill_worker(&session.worker);
                        }
                        Err(error) => {
                            append_service_diagnostic_log(&format!(
                                "worker_shutdown_request_failed worker_id={} session_id={} capability={} error={}",
                                sanitize_log_value(session.worker.worker_id.as_str()),
                                sanitize_log_value(session.session_id.id.as_str()),
                                sanitize_log_value(&session.capability),
                                sanitize_log_value(&error.to_string())
                            ));
                            let _ = self.supervisor.kill_worker(&session.worker);
                        }
                    }
                    self.mark_session_closed_for_shutdown(&session);
                    return;
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    append_service_diagnostic_log(&format!(
                        "worker_shutdown_lock_poisoned worker_id={} session_id={}",
                        sanitize_log_value(session.worker.worker_id.as_str()),
                        sanitize_log_value(session.session_id.id.as_str())
                    ));
                    let _ = self.supervisor.kill_worker(&session.worker);
                    self.mark_session_closed_for_shutdown(&session);
                    return;
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if started.elapsed() >= timeout {
                        append_service_diagnostic_log(&format!(
                            "worker_shutdown_lock_timeout worker_id={} session_id={} timeout_ms={}",
                            sanitize_log_value(session.worker.worker_id.as_str()),
                            sanitize_log_value(session.session_id.id.as_str()),
                            timeout.as_millis()
                        ));
                        let _ = self.supervisor.kill_worker(&session.worker);
                        self.mark_session_closed_for_shutdown(&session);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }

    pub(super) fn mark_session_closed_for_shutdown(&self, session: &ManagedSession) {
        let Ok(mut state) = self.lock_state() else {
            return;
        };
        if let Some(current) = state.sessions.get_mut(session.session_id.id.as_str()) {
            current.state = DebugSessionState::Closed;
            current.updated_at = Timestamp::now();
        }
    }

    pub fn handle_rpc(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();
        let result = match method_spec(&request.method) {
            Some(spec) => self.dispatch_registered_method(spec, request.params),
            None => Err(ServiceError::Rpc(format!(
                "unknown method `{}`",
                request.method
            ))),
        };

        match result {
            Ok(value) => JsonRpcResponse::result(id, value),
            Err(error) => JsonRpcResponse::error(id, rpc_error_for(error)),
        }
    }

    pub(super) fn dispatch_registered_method(
        &self,
        spec: MethodSpec,
        params: Option<Value>,
    ) -> Result<Value, ServiceError> {
        let name = spec.name.as_str();
        match spec.route {
            MethodRoute::Service => match name {
                "service.health" => self.service_health(),
                "service.info" => self.service_info(),
                "service.update" => self.service_update(params),
                _ => unhandled_registered_method(name),
            },
            MethodRoute::Session => match name {
                "session.list" => self.session_list(params),
                _ => unhandled_registered_method(name),
            },
            MethodRoute::Debug => match name {
                "debug.session.create" => self.debug_session_create(params),
                "debug.session.close" => self.debug_session_close(params),
                "debug.session.kill" => self.debug_session_kill(params),
                "debug.eval" => self.debug_eval(params),
                "debug.eval_steps" => self.debug_eval_steps(params),
                "debug.modules" => self.debug_builtin_eval(params, "debug.modules", "lm"),
                "debug.threads" => self.debug_builtin_eval(params, "debug.threads", "~"),
                "debug.stack" => self.debug_builtin_eval(params, "debug.stack", "k"),
                "debug.add_symbols" => self.debug_add_symbols(params),
                "debug.read_memory" => self.debug_read_memory(params),
                _ => unhandled_registered_method(name),
            },
            MethodRoute::Reverse => match name {
                "reverse.session.open" => self.reverse_session_open(params),
                "reverse.lookup_function" => self.reverse_lookup_function(params),
                "reverse.session.close" => self.reverse_session_close(params),
                _ => self.reverse_core_function(
                    name.strip_prefix("reverse.")
                        .expect("reverse method has reverse namespace"),
                    params,
                ),
            },
            MethodRoute::Recording => match name {
                "recording.start" => self.recording_start(params),
                "recording.ttd" => self.recording_ttd(params),
                "recording.status" => self.recording_status(params),
                "recording.stop" => self.recording_stop(params),
                "recording.cancel" => self.recording_cancel(params),
                "recording.kill" => self.recording_kill(params),
                _ => unhandled_registered_method(name),
            },
            MethodRoute::Operation => match name {
                "operation.get" => self.operation_get(params),
                "operation.cancel" => self.operation_cancel(params),
                "operation.stream" => self.operation_stream(params),
                "operation.list" => self.operation_list(params),
                _ => unhandled_registered_method(name),
            },
            MethodRoute::Artifact => match name {
                "artifact.list" => self.artifact_list(params),
                _ => unhandled_registered_method(name),
            },
            MethodRoute::Audit => match name {
                "audit.list_commands" => self.audit_list_commands(params),
                _ => unhandled_registered_method(name),
            },
        }
    }

    pub fn handle_mcp(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let context = DiagnosticContext::from_rpc(&request);
        if request.id.is_none() {
            if let Err(error) = self.handle_mcp_method(request) {
                append_service_diagnostic_log(&format!(
                    "mcp_dispatch_error {} error={}",
                    context.log_fields(),
                    sanitize_log_value(&error.to_string())
                ));
            }
            return None;
        }
        let id = request.id.clone();
        Some(match self.handle_mcp_method(request) {
            Ok(result) => JsonRpcResponse::result(id, result),
            Err(error) => {
                append_service_diagnostic_log(&format!(
                    "mcp_dispatch_error {} error={}",
                    context.log_fields(),
                    sanitize_log_value(&error.to_string())
                ));
                JsonRpcResponse::error(id, mcp_error_for(error))
            }
        })
    }

    pub(super) fn handle_mcp_method(&self, request: JsonRpcRequest) -> Result<Value, ServiceError> {
        match request.method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "dbgatlas-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "tools": {},
                },
            })),
            "ping" => Ok(json!({})),
            "notifications/initialized" => Ok(json!(null)),
            "tools/list" => Ok(json!({ "tools": mcp_tool_descriptors() })),
            "tools/call" => {
                let params: ToolCallParams =
                    serde_json::from_value(request.params.unwrap_or_else(|| json!({})))?;
                let arguments = params.arguments.unwrap_or_else(|| json!({}));
                let result = self.call_mcp_tool_output(&params.name, arguments.clone())?;
                if result.is_error {
                    append_mcp_tool_error_log(&params.name, &arguments, &result);
                }
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result.value)?,
                    }],
                    "isError": result.is_error,
                }))
            }
            other => Err(ServiceError::Rpc(format!("unknown MCP method `{other}`"))),
        }
    }

    pub(super) fn call_mcp_tool_output(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<ToolCallOutput, ServiceError> {
        let spec = method_spec(name)
            .ok_or_else(|| ServiceError::Rpc(format!("unknown DbgAtlas tool `{name}`")))?;
        self.call_mcp_service_tool(&spec.name, arguments)
    }

    pub(super) fn call_mcp_service_tool(
        &self,
        method: &str,
        arguments: Value,
    ) -> Result<ToolCallOutput, ServiceError> {
        if !arguments.is_object() {
            return Err(ServiceError::Rpc(
                "tool arguments must be a JSON object".to_string(),
            ));
        }
        let response = self.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params: Some(arguments.clone()),
        });
        let mut output = mcp_service_response_result(response);
        if let Some(operation_id) = output
            .operation_id
            .clone()
            .or_else(|| self.latest_operation_id_from_arguments(&arguments))
        {
            output.set_operation_id(operation_id);
        }
        Ok(output)
    }

    pub(super) fn service_health(&self) -> Result<Value, ServiceError> {
        Ok(json!({
            "status": "ok",
            "service": "DbgAtlas",
            "version": env!("CARGO_PKG_VERSION"),
        }))
    }

    pub(super) fn session_list(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        if let Some(params) = params {
            if !params.is_object() {
                return Err(ServiceError::Rpc(
                    "session.list arguments must be a JSON object".to_string(),
                ));
            }
        }
        let mut sessions = {
            let state = self.lock_state()?;
            state
                .sessions
                .values()
                .map(SessionListItem::from)
                .collect::<Vec<_>>()
        };
        sessions.sort_by(|left, right| {
            left.session_id
                .id
                .as_str()
                .cmp(right.session_id.id.as_str())
        });
        Ok(json!({
            "count": sessions.len(),
            "sessions": sessions,
        }))
    }

    pub(super) fn service_info(&self) -> Result<Value, ServiceError> {
        let state = self.lock_state()?;
        Ok(json!({
            "service": "DbgAtlas",
            "version": env!("CARGO_PKG_VERSION"),
            "session_count": state.sessions.len(),
            "operation_count": state.operations.len(),
            "external_api": "json-rpc-2.0-over-http",
        }))
    }

    pub(super) fn service_update(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: ServiceUpdateParams = parse_params(params)?;
        let accepted = request_windows_service_update(WindowsServiceUpdateOptions {
            source_dir: params.source_dir,
            restart: params.restart,
            timeout_ms: params.timeout_ms,
        })?;
        Ok(serde_json::to_value(accepted)?)
    }

    pub(super) fn operation_get(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: OperationGetParams = parse_params(params)?;
        let state = self.lock_state()?;
        let operation = state
            .operations
            .get(params.operation_id.id.as_str())
            .ok_or_else(|| ServiceError::OperationNotFound(params.operation_id.to_string()))?;
        Ok(serde_json::to_value(operation)?)
    }

    pub(super) fn operation_cancel(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: OperationGetParams = parse_params(params)?;
        let (operation, session) = {
            let mut state = self.lock_state()?;
            let operation = state
                .operations
                .get_mut(params.operation_id.id.as_str())
                .ok_or_else(|| ServiceError::OperationNotFound(params.operation_id.to_string()))?;
            if operation.status != ServiceOperationStatus::Running {
                return Err(ServiceError::OperationNotCancelable(
                    params.operation_id.to_string(),
                ));
            }
            operation.status = ServiceOperationStatus::Canceled;
            operation.summary = "operation canceled".to_string();
            operation.updated_at = Timestamp::now();
            operation.events.push(ServiceEvent {
                timestamp: Timestamp::now(),
                kind: "canceled".to_string(),
                message: "operation canceled by request".to_string(),
            });
            let operation = operation.clone();
            let session = operation
                .session_id
                .as_ref()
                .and_then(|session_id| state.sessions.get(session_id.id.as_str()).cloned());
            (operation, session)
        };

        if let Some(session) = session {
            let cancel_outcome = self.supervisor.cancel_worker_operation(
                &session.worker,
                &session.session_id,
                &params.operation_id,
            )?;
            if cancel_outcome == WorkerCancelOutcome::WorkerKilled {
                let mut state = self.lock_state()?;
                if let Some(session) = state.sessions.get_mut(session.session_id.id.as_str()) {
                    session.state = DebugSessionState::Error;
                    session.updated_at = Timestamp::now();
                }
            }
            let workspace = Workspace::open(&session.internal_workspace_root)?;
            workspace.append_operation(&OperationRecord {
                operation_id: params.operation_id,
                adapter_id: "service".to_string(),
                capability: operation.capability.clone(),
                status: OperationStatus::Canceled,
                created_at: Timestamp::now(),
                summary: "operation canceled".to_string(),
                artifacts: operation.artifacts.clone(),
                raw_output: operation.raw_output.clone(),
            })?;
        }

        Ok(serde_json::to_value(operation)?)
    }

    pub(super) fn operation_stream(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: OperationGetParams = parse_params(params)?;
        let state = self.lock_state()?;
        let operation = state
            .operations
            .get(params.operation_id.id.as_str())
            .ok_or_else(|| ServiceError::OperationNotFound(params.operation_id.to_string()))?;
        Ok(json!({
            "operation_id": operation.operation_id,
            "events": operation.events,
        }))
    }

    pub(super) fn artifact_list(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let facts = project_facts(params)?;
        Ok(json!({ "artifacts": facts.artifacts }))
    }

    pub(super) fn operation_list(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let facts = project_facts(params)?;
        Ok(json!({ "operations": facts.operations }))
    }

    pub(super) fn audit_list_commands(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let facts = project_facts(params)?;
        Ok(json!({ "commands": facts.command_audit }))
    }
}

fn project_facts(params: Option<Value>) -> Result<WorkspaceFacts, ServiceError> {
    let params: ProjectRootParams = parse_params(params)?;
    workspace_facts_with_fallback(&params.project_root)
}

fn unhandled_registered_method(name: &str) -> Result<Value, ServiceError> {
    Err(ServiceError::Rpc(format!(
        "registered method `{name}` has no handler"
    )))
}
