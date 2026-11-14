use super::*;
use std::thread;

impl ServiceHost {
    pub fn new(supervisor: Arc<dyn WorkerSupervisor>) -> Self {
        Self {
            service_instance: format!(
                "service-{}-{}",
                std::process::id(),
                Timestamp::now().unix_millis
            ),
            state: Arc::new(Mutex::new(ServiceState::default())),
            supervisor,
            capabilities: ServiceCapabilities::from_runtime_config(&RuntimeConfig::default()),
            #[cfg(test)]
            ttd_runner: Arc::new(ProcessTtdRecorderRunner),
            #[cfg(test)]
            etw_controller: Arc::new(NativeEtwController),
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

    #[cfg(test)]
    pub(super) fn with_etw_controller(mut self, controller: Arc<dyn EtwController>) -> Self {
        self.etw_controller = controller;
        self
    }

    pub(super) fn shutdown_workers_gracefully(&self, timeout: Duration) {
        let (debug, recordings) = match self.lock_state() {
            Ok(state) => (
                state.sessions.values().cloned().collect::<Vec<_>>(),
                state
                    .remote_recordings
                    .values()
                    .filter(|r| {
                        !state
                            .summaries
                            .get(r.session_id.id.as_str())
                            .is_some_and(|s| s.cleanup.complete)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            Err(_) => return,
        };
        for session in debug {
            self.shutdown_session_worker(session, timeout);
        }
        for recording in recordings {
            let host = self.clone();
            let id = recording.session_id.clone();
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(host.recording_stop(Some(json!({"recording_id":id}))));
            });
            if !matches!(rx.recv_timeout(timeout), Ok(Ok(_))) {
                let cleanup = self.supervisor.kill_worker(&recording.worker);
                let _ = self.set_session_terminal(
                    &recording.session_id,
                    true,
                    cleanup.is_ok(),
                    Some("service shutdown forced recording cleanup".into()),
                );
                if cleanup.is_ok() {
                    let _ = self.release_recording_slot(&recording);
                }
            }
        }
    }

    pub(super) fn shutdown_session_worker(&self, session: ManagedSession, timeout: Duration) {
        let host = self.clone();
        let id = session.session_id.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            loop {
                let result = host.debug_session_close(Some(json!({"session_id":id})));
                if matches!(result, Err(ServiceError::SessionBusy(_))) && Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(25));
                    continue;
                }
                let _ = tx.send(result);
                break;
            }
        });
        if !matches!(rx.recv_timeout(timeout), Ok(Ok(_))) {
            let cleanup = self.supervisor.kill_worker(&session.worker);
            let _ = self.set_session_terminal(
                &session.session_id,
                true,
                cleanup.is_ok(),
                Some("service shutdown forced worker cleanup".into()),
            );
            if cleanup.is_ok() {
                if let Ok(mut state) = self.lock_state() {
                    state.sessions.remove(session.session_id.id.as_str());
                }
            }
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
        let incoming_id = params.as_ref().and_then(|p| {
            extract_ref_id_field(p, "session_id")
                .or_else(|| extract_ref_id_field(p, "recording_id"))
        });
        let observe = matches!(
            spec.route,
            MethodRoute::Debug | MethodRoute::Recording | MethodRoute::Operation
        );
        let mut result = self.dispatch_method_inner(spec, params);
        if observe {
            let session_id = result
                .as_ref()
                .ok()
                .and_then(|r| {
                    extract_ref_id_field(r, "session_id")
                        .or_else(|| extract_ref_id_field(r, "recording_id"))
                })
                .or(incoming_id);
            if let Some(id) = session_id {
                let summary = self.session_get(Some(json!({"session_id":id})))?;
                let session: dbgatlas_model::Session = serde_json::from_value(summary.clone())?;
                if let Some(operation_id) = &session.last_operation_id {
                    if let Some(record) =
                        Workspace::open(&session.workspace.root)?.get_operation(operation_id)?
                    {
                        let mut state = self.lock_state()?;
                        if let Some(operation) = state.operations.get_mut(operation_id.id.as_str())
                        {
                            if operation.details.output_error.is_none()
                                || record.status == operation.status
                            {
                                operation.details = record.details;
                                operation.status = record.status;
                            }
                        }
                    }
                }
                if let Ok(value) = &mut result {
                    value["session"] = summary;
                }
            }
        }
        result
    }

    fn dispatch_method_inner(
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
                "session.get" => self.session_get(params),
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
        let params = params.unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Err(ServiceError::Rpc(
                "session.list arguments must be an object".into(),
            ));
        }
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("debug");
        if !["debug", "recording", "all"].contains(&kind) {
            return Err(ServiceError::Rpc("invalid session kind".into()));
        }
        let include_terminal = params
            .get("include_terminal")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let ids: Vec<_> = self
            .lock_state()?
            .summaries
            .values()
            .filter(|s| {
                (kind == "all"
                    || (kind == "debug") == (s.kind == dbgatlas_model::SessionKind::Debug))
                    && (include_terminal || !s.lifecycle.is_terminal())
            })
            .map(|s| s.session_id.clone())
            .collect();
        let mut sessions = Vec::new();
        for id in ids {
            let summary = self.session_get(Some(json!({"session_id":id})))?;
            if !include_terminal
                && matches!(summary["lifecycle"].as_str(), Some("closed" | "failed"))
            {
                continue;
            }
            let state = self.lock_state()?;
            let mut item = if let Some(session) = state.sessions.get(id.id.as_str()) {
                serde_json::to_value(SessionListItem::from(session))?
            } else {
                summary.clone()
            };
            item["session"] = summary;
            sessions.push(item);
        }
        sessions.sort_by_key(|s| s["session_id"].to_string());
        Ok(json!({"count":sessions.len(), "sessions":sessions}))
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
        let (original, session) = {
            let state = self.lock_state()?;
            let operation = state
                .operations
                .get(params.operation_id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::OperationNotFound(params.operation_id.to_string()))?;
            if operation.status != ServiceOperationStatus::Running {
                return Err(ServiceError::OperationNotCancelable(
                    params.operation_id.to_string(),
                ));
            }
            let session = operation
                .session_id
                .as_ref()
                .and_then(|id| state.sessions.get(id.id.as_str()))
                .cloned();
            (operation, session)
        };
        let session = session.ok_or_else(|| {
            ServiceError::Rpc(
                "operation cancellation is unsupported; use the recording stop/kill action".into(),
            )
        })?;
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let cancellation_id = next_operation_ref();
        self.begin_operation_record(
            &workspace,
            &cancellation_id,
            &session.session_id,
            "operation.cancel",
            json!({"operation_id":params.operation_id}),
        )?;
        let outcome = match self.supervisor.cancel_worker_operation(
            &session.worker,
            &session.session_id,
            &params.operation_id,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.append_operation_result(
                    &workspace,
                    &OperationRecord {
                        details: dbgatlas_model::OperationDetails {
                            related_operation_id: Some(params.operation_id),
                            ..Default::default()
                        },
                        operation_id: cancellation_id,
                        adapter_id: "service".into(),
                        capability: "operation.cancel".into(),
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
        if outcome == WorkerCancelOutcome::WorkerKilled {
            self.mark_debug_session_error(&session.session_id)?;
            self.finish_operation_in_memory(
                &params.operation_id,
                ServiceOperationStatus::Unknown,
                "worker terminated; original operation outcome unknown".into(),
                original.artifacts.clone(),
                original.raw_output.clone(),
            )?;
            self.append_operation_result(
                &workspace,
                &OperationRecord {
                    details: Default::default(),
                    operation_id: params.operation_id.clone(),
                    adapter_id: "service".into(),
                    capability: original.capability,
                    status: OperationStatus::Unknown,
                    created_at: Timestamp::now(),
                    summary: "worker terminated; outcome unknown".into(),
                    artifacts: original.artifacts,
                    raw_output: original.raw_output,
                },
            )?;
        }
        self.append_operation_result(
            &workspace,
            &OperationRecord {
                details: dbgatlas_model::OperationDetails {
                    related_operation_id: Some(params.operation_id.clone()),
                    ..Default::default()
                },
                operation_id: cancellation_id.clone(),
                adapter_id: "service".into(),
                capability: "operation.cancel".into(),
                status: OperationStatus::Success,
                created_at: Timestamp::now(),
                summary: "cancellation request delivered; original result reported separately"
                    .into(),
                artifacts: vec![],
                raw_output: None,
            },
        )?;
        self.finish_operation_in_memory(
            &cancellation_id,
            ServiceOperationStatus::Success,
            "cancellation request delivered".into(),
            vec![],
            None,
        )?;
        let mut result = self.operation_get(Some(json!({"operation_id":params.operation_id})))?;
        result["cancellation_operation_id"] = serde_json::to_value(cancellation_id)?;
        result["cancellation_requested"] = json!(true);
        Ok(result)
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
