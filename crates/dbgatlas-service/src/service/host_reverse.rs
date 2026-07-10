use super::*;

impl ServiceHost {
    pub(super) fn reverse_session_open(
        &self,
        params: Option<Value>,
    ) -> Result<Value, ServiceError> {
        let params: ReverseSessionOpenParams = parse_params(params)?;
        let session_id = next_session_ref();
        let operation_id = next_operation_ref();
        let workspace = ensure_project_workspace(&params.project_root)?;
        let artifact_dir = workspace.ensure_reverse_session_artifact_dir(&session_id.id)?;
        let ida_install_dir = params
            .ida_install_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_IDA_INSTALL_DIR));
        let worker = match self.supervisor.create_worker(WorkerCreateRequest {
            session_id: session_id.clone(),
            project_root: params.project_root.clone(),
            internal_workspace_root: workspace.root().to_path_buf(),
            artifact_dir: artifact_dir.clone(),
            startup_timeout_ms: 5_000,
            dbgeng_dirs: self.capabilities.dbgeng_dirs.clone(),
            identity: Some(WorkerIdentity::ActiveInteractiveUser),
        }) {
            Ok(worker) => worker,
            Err(error) => {
                let message = diagnose_reverse_open_error(format!(
                    "failed to create reverse worker: {error}"
                ));
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &session_id,
                    &artifact_dir,
                    &operation_id,
                    "reverse.session.open",
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.open".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(message));
            }
        };

        let open = self.supervisor.request_worker(
            &worker,
            WorkerRequest::OpenReverseSession {
                session_id: session_id.clone(),
                ida_install_dir: ida_install_dir.clone(),
                database_path: params.database_path.clone(),
            },
        );
        match open {
            Ok(WorkerResponse::ReverseSessionOpened { .. }) => {}
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let _ = self.supervisor.kill_worker(&worker);
                let error = diagnose_reverse_open_error(worker_failed_message(code, message));
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &session_id,
                    &artifact_dir,
                    &operation_id,
                    "reverse.session.open",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.open".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Ok(other) => {
                let _ = self.supervisor.kill_worker(&worker);
                let error = diagnose_reverse_open_error(format!(
                    "unexpected reverse open response: {other:?}"
                ));
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &session_id,
                    &artifact_dir,
                    &operation_id,
                    "reverse.session.open",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.open".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Err(error) => {
                let _ = self.supervisor.kill_worker(&worker);
                let message = diagnose_reverse_open_error(error.to_string());
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &session_id,
                    &artifact_dir,
                    &operation_id,
                    "reverse.session.open",
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.open".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message,
                    artifacts,
                    None,
                )?;
                return Err(error);
            }
        };

        let result = (|| {
            let now = Timestamp::now();
            let metadata = json!({
                "session_id": session_id,
                "database_path": params.database_path,
                "ida_install_dir": ida_install_dir,
                "created_at": now,
                "mode": "native_dynamic_idalib",
                "writes_idb": false,
                "open_operation_writes_idb": false,
                "session_write_capable": true
            });
            let session_metadata_file = "sessions/session.json";
            let byte_len = write_json_file(&artifact_dir.join(session_metadata_file), &metadata)?;
            let artifact_id = next_artifact_ref();
            workspace.register_artifact(&ArtifactMetadata {
                artifact_id: artifact_id.clone(),
                kind: "reverse.session".to_string(),
                relative_path: reverse_relative_path(&session_id, session_metadata_file),
                created_at: now,
                operation_id: Some(operation_id.clone()),
                byte_len: Some(byte_len),
                description: Some("IDA reverse session metadata".to_string()),
            })?;
            workspace.append_operation(&OperationRecord {
                operation_id: operation_id.clone(),
                adapter_id: "ida".to_string(),
                capability: "reverse.session.open".to_string(),
                status: OperationStatus::Success,
                created_at: now,
                summary: "reverse session opened".to_string(),
                artifacts: vec![artifact_id.clone()],
                raw_output: None,
            })?;

            let session = ManagedSession {
                session_id: session_id.clone(),
                capability: "reverse".to_string(),
                project_root: params.project_root,
                internal_workspace_root: workspace.root().to_path_buf(),
                artifact_dir,
                target: None,
                database_path: Some(params.database_path),
                ida_install_dir: Some(ida_install_dir),
                state: DebugSessionState::Ready,
                worker: worker.clone(),
                request_lock: Arc::new(Mutex::new(())),
                created_at: now,
                updated_at: now,
                last_operation: Some(operation_id.clone()),
                artifacts: vec![artifact_id.clone()],
            };

            let mut operation = ServiceOperation::success(
                operation_id.clone(),
                "reverse.session.open",
                Some(session_id.clone()),
                "reverse session opened",
            );
            operation.artifacts = vec![artifact_id.clone()];
            let mut state = self.lock_state()?;
            state
                .operations
                .insert(operation_id.id.as_str().to_string(), operation);
            state
                .sessions
                .insert(session_id.id.as_str().to_string(), session.clone());

            Ok(json!({
                "session_id": session_id,
                "operation_id": operation_id,
                "operation_status": "success",
                "artifact_refs": [artifact_id],
                "operation": {
                    "status": "success",
                    "artifact_refs": [artifact_id],
                    "raw_output_ref": null
                }
            }))
        })();
        if result.is_err() {
            let close = self
                .supervisor
                .request_worker(&worker, WorkerRequest::CloseReverseSession { session_id });
            if matches!(close, Ok(WorkerResponse::Ok { .. })) {
                let _ = self.supervisor.close_worker(&worker);
            } else {
                let _ = self.supervisor.kill_worker(&worker);
            }
        }
        result
    }

    pub(super) fn reverse_lookup_function(
        &self,
        params: Option<Value>,
    ) -> Result<Value, ServiceError> {
        let params: ReverseLookupFunctionParams = parse_params(params)?;
        let runtime_address = parse_u64_param(&params.runtime_address, "runtime_address")?;
        let runtime_module_base =
            parse_u64_param(&params.runtime_module_base, "runtime_module_base")?;
        let ida_image_base = parse_u64_param(&params.ida_image_base, "ida_image_base")?;
        let session = self.reusable_reverse_session(&params.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let lookup = {
            let _request_guard = session
                .request_lock
                .lock()
                .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
            self.supervisor.request_worker(
                &session.worker,
                WorkerRequest::LookupReverseFunction {
                    session_id: params.session_id.clone(),
                    runtime_address,
                    runtime_module_base,
                    ida_image_base,
                },
            )
        };
        let lookup = match lookup {
            Ok(WorkerResponse::ReverseFunctionLookup { result, .. }) => result,
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let error = worker_failed_message(code, message);
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.lookup_function",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.lookup_function".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Ok(other) => {
                let error = format!("unexpected reverse lookup response: {other:?}");
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.lookup_function",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.lookup_function".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Err(error) => {
                let message = error.to_string();
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.lookup_function",
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.lookup_function".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message,
                    artifacts,
                    None,
                )?;
                return Err(error);
            }
        };
        let now = Timestamp::now();
        let event = json!({
            "operation_id": operation_id,
            "session_id": params.session_id,
            "created_at": now,
            "lookup": lookup,
        });
        let lookup_file = format!("lookups/{}.jsonl", operation_id.id.as_str());
        let byte_len = write_jsonl_file(&session.artifact_dir.join(&lookup_file), &event)?;
        let artifact_id = next_artifact_ref();
        workspace.register_artifact(&ArtifactMetadata {
            artifact_id: artifact_id.clone(),
            kind: "reverse.lookup".to_string(),
            relative_path: reverse_relative_path(&params.session_id, &lookup_file),
            created_at: now,
            operation_id: Some(operation_id.clone()),
            byte_len: Some(byte_len),
            description: Some("IDA function lookup result".to_string()),
        })?;
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "ida".to_string(),
            capability: "reverse.lookup_function".to_string(),
            status: OperationStatus::Success,
            created_at: now,
            summary: if lookup.found {
                "IDA function located".to_string()
            } else {
                "IDA function not found".to_string()
            },
            artifacts: vec![artifact_id.clone()],
            raw_output: None,
        })?;

        let mut operation = ServiceOperation::success(
            operation_id.clone(),
            "reverse.lookup_function",
            Some(params.session_id.clone()),
            "reverse lookup completed",
        );
        operation.artifacts = vec![artifact_id.clone()];
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        if let Some(session) = state.sessions.get_mut(params.session_id.id.as_str()) {
            session.updated_at = now;
            session.last_operation = Some(operation_id.clone());
            session.artifacts.push(artifact_id.clone());
        }

        Ok(json!({
            "session_id": params.session_id,
            "operation_id": operation_id,
            "operation_status": "success",
            "artifact_refs": [artifact_id],
            "runtime_address": lookup.runtime_address,
            "runtime_module_base": lookup.runtime_module_base,
            "rva": lookup.rva,
            "ida_image_base": lookup.ida_image_base,
            "ida_ea": lookup.ida_ea,
            "function_start": lookup.function_start,
            "function_end": lookup.function_end,
            "function_name": lookup.function_name,
            "found": lookup.found,
            "operation": {
                "status": "success",
                "artifact_refs": [artifact_id],
                "raw_output_ref": null
            }
        }))
    }

    pub(super) fn reverse_core_function(
        &self,
        function: &str,
        params: Option<Value>,
    ) -> Result<Value, ServiceError> {
        let params: ReverseCoreFunctionParams = parse_params(params)?;
        let session = self.reusable_reverse_session(&params.session_id)?;

        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let arguments = Value::Object(params.arguments.into_iter().collect());
        let capability = format!("reverse.{function}");
        let core = {
            let _request_guard = session
                .request_lock
                .lock()
                .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
            self.supervisor.request_worker(
                &session.worker,
                WorkerRequest::ReverseCoreFunction {
                    session_id: params.session_id.clone(),
                    function: function.to_string(),
                    arguments: arguments.clone(),
                },
            )
        };
        let core = match core {
            Ok(WorkerResponse::ReverseCoreFunction { result, .. }) => result,
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let error = worker_failed_message(code, message);
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    &capability,
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: capability.clone(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.record_failed_operation_in_memory(
                    &operation_id,
                    &capability,
                    Some(params.session_id.clone()),
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Ok(other) => {
                let error = format!("unexpected reverse core response: {other:?}");
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    &capability,
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: capability.clone(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.record_failed_operation_in_memory(
                    &operation_id,
                    &capability,
                    Some(params.session_id.clone()),
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Err(error) => {
                let message = error.to_string();
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    &capability,
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: capability.clone(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.record_failed_operation_in_memory(
                    &operation_id,
                    &capability,
                    Some(params.session_id.clone()),
                    message,
                    artifacts,
                    None,
                )?;
                return Err(error);
            }
        };
        record_successful_reverse_core_operation(
            self,
            &workspace,
            &session,
            &params.session_id,
            &operation_id,
            &capability,
            arguments,
            core,
        )
    }

    pub(super) fn reverse_session_close(
        &self,
        params: Option<Value>,
    ) -> Result<Value, ServiceError> {
        let params: ReverseSessionCloseParams = parse_params(params)?;
        let session = self.reusable_reverse_session(&params.session_id)?;
        let operation_id = next_operation_ref();
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let close = {
            let _request_guard = session
                .request_lock
                .lock()
                .map_err(|_| ServiceError::Rpc("session request lock poisoned".to_string()))?;
            self.supervisor.request_worker(
                &session.worker,
                WorkerRequest::CloseReverseSession {
                    session_id: params.session_id.clone(),
                },
            )
        };
        match close {
            Ok(WorkerResponse::Ok { .. }) => {
                self.supervisor.close_worker(&session.worker)?;
            }
            Ok(WorkerResponse::Failed { code, message, .. }) => {
                let error = worker_failed_message(code, message);
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.session.close",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.close".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Ok(other) => {
                let error = format!("unexpected reverse close response: {other:?}");
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.session.close",
                    &error,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.close".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: error.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    error.clone(),
                    artifacts,
                    None,
                )?;
                return Err(ServiceError::Worker(error));
            }
            Err(error) => {
                let message = error.to_string();
                let artifacts = record_failed_reverse_operation(
                    &workspace,
                    &params.session_id,
                    &session.artifact_dir,
                    &operation_id,
                    "reverse.session.close",
                    &message,
                )?;
                workspace.append_operation(&OperationRecord {
                    operation_id: operation_id.clone(),
                    adapter_id: "ida".to_string(),
                    capability: "reverse.session.close".to_string(),
                    status: OperationStatus::Failed,
                    created_at: Timestamp::now(),
                    summary: message.clone(),
                    artifacts: artifacts.clone(),
                    raw_output: None,
                })?;
                self.finish_operation_in_memory(
                    &operation_id,
                    ServiceOperationStatus::Failed,
                    message,
                    artifacts,
                    None,
                )?;
                return Err(error);
            }
        }
        workspace.append_operation(&OperationRecord {
            operation_id: operation_id.clone(),
            adapter_id: "ida".to_string(),
            capability: "reverse.session.close".to_string(),
            status: OperationStatus::Success,
            created_at: Timestamp::now(),
            summary: "reverse session closed".to_string(),
            artifacts: Vec::new(),
            raw_output: None,
        })?;
        let operation = ServiceOperation::success(
            operation_id.clone(),
            "reverse.session.close",
            Some(params.session_id.clone()),
            "reverse session closed",
        );
        let mut state = self.lock_state()?;
        state
            .operations
            .insert(operation_id.id.as_str().to_string(), operation);
        state.sessions.remove(params.session_id.id.as_str());
        Ok(json!({
            "session_id": params.session_id,
            "operation_id": operation_id,
            "operation_status": "success",
            "operation": {
                "status": "success",
                "artifact_refs": [],
                "raw_output_ref": null
            }
        }))
    }
}
