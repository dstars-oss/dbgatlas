use super::*;
use dbgatlas_model::{
    CleanupResult, ClosePolicy, ConnectionState, Environment, EnvironmentPath, ExecutionContext,
    OperationDetails, ProcessOwnership, Session, SessionKind, SessionLifecycle, StateObservation,
    TargetBinding, TargetState, WorkerBinding, WorkspaceRef,
};

impl ServiceHost {
    pub(super) fn open_session_record(
        &self,
        session_id: &SessionRef,
        workspace: &Workspace,
        kind: SessionKind,
        target: Value,
        operation_id: &OperationRef,
        capability: &str,
        input: Value,
    ) -> Result<(), ServiceError> {
        let env = Environment::local(&self.service_instance);
        let target_kind = target
            .get("kind")
            .or_else(|| target.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let file = target_kind == "file";
        let location = target
            .get("path")
            .and_then(Value::as_str)
            .map(|path| EnvironmentPath {
                environment_id: env.id.clone(),
                path: path.into(),
            });
        let now = Timestamp::now();
        let session = Session {
            schema_version: 1,
            session_id: session_id.clone(),
            kind,
            workspace: WorkspaceRef::new(workspace.root()),
            service_instance: self.service_instance.clone(),
            control_environment: env.clone(),
            lifecycle: SessionLifecycle::Opening,
            connection: ConnectionState::Unknown,
            worker: None,
            target: TargetBinding {
                revision: 1,
                kind: if file {
                    "file"
                } else if target_kind == "monitor" {
                    "monitor"
                } else {
                    "process"
                }
                .into(),
                environment: (!file).then_some(env),
                process: None,
                location,
                created_by_session: target_kind == "launch",
                open_request: target,
            },
            target_state: StateObservation {
                state: if file {
                    TargetState::Offline
                } else {
                    TargetState::Unknown
                },
                last_known: None,
                source: "open_request".into(),
                observed_at: now,
            },
            context: json!({}),
            capabilities: vec![],
            policy: ClosePolicy::default(),
            cleanup: CleanupResult::default(),
            created_at: now,
            updated_at: now,
            last_operation_id: Some(operation_id.clone()),
            artifacts: vec![],
            error: None,
            warnings: vec![],
        };
        self.persist_session(&session)?;
        self.lock_state()?
            .summaries
            .insert(session_id.to_string(), session);
        self.begin_operation_record(workspace, operation_id, session_id, capability, input)
    }

    pub(super) fn begin_operation_record(
        &self,
        workspace: &Workspace,
        operation_id: &OperationRef,
        session_id: &SessionRef,
        capability: &str,
        input: Value,
    ) -> Result<(), ServiceError> {
        // Refresh backend facts before capturing the immutable operation context.
        if self
            .lock_state()?
            .sessions
            .contains_key(session_id.id.as_str())
        {
            self.session_get(Some(json!({"session_id":session_id})))?;
        }
        let snapshot = {
            let mut state = self.lock_state()?;
            if let Some(session) = state.summaries.get_mut(session_id.id.as_str()) {
                session.last_operation_id = Some(operation_id.clone());
                session.updated_at = Timestamp::now();
            }
            state.summaries.get(session_id.id.as_str()).cloned()
        };
        let details = OperationDetails {
            session_id: Some(session_id.clone()),
            input: Some(input),
            execution_context: snapshot
                .filter(|s| s.worker.is_some())
                .map(|s| ExecutionContext {
                    worker: s.worker,
                    target: s.target,
                    context: s.context,
                }),
            ..Default::default()
        };
        workspace.append_operation(&OperationRecord {
            details: details.clone(),
            operation_id: operation_id.clone(),
            adapter_id: "service".into(),
            capability: capability.into(),
            status: if details.execution_context.is_some() {
                OperationStatus::Running
            } else {
                OperationStatus::Pending
            },
            created_at: Timestamp::now(),
            summary: format!("{capability} accepted"),
            artifacts: vec![],
            raw_output: None,
        })?;
        let mut operation = ServiceOperation::running(
            operation_id.clone(),
            capability,
            Some(session_id.clone()),
            "accepted",
        );
        operation.details = details;
        self.lock_state()?
            .operations
            .insert(operation_id.to_string(), operation);
        Ok(())
    }

    pub(super) fn start_open_operation(
        &self,
        workspace: &Workspace,
        operation_id: &OperationRef,
        session_id: &SessionRef,
        worker: &WorkerHandle,
        backend: &str,
    ) -> Result<(), ServiceError> {
        let session = self
            .lock_state()?
            .summaries
            .get(session_id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::SessionNotFound(session_id.to_string()))?;
        let binding = WorkerBinding {
            worker_id: worker.worker_id.clone(),
            environment: session.control_environment.clone(),
            process: self.supervisor.process_instance(worker),
            backend: backend.into(),
            execution_identity: serde_json::to_value(&worker.identity)?
                .as_str()
                .unwrap_or("unknown")
                .into(),
            ownership: ProcessOwnership::Managed,
        };
        let mut record = workspace
            .get_operation(operation_id)?
            .ok_or_else(|| ServiceError::OperationNotFound(operation_id.to_string()))?;
        record.status = OperationStatus::Running;
        record.details.execution_context = Some(ExecutionContext {
            worker: Some(binding),
            target: session.target,
            context: session.context,
        });
        self.append_operation_result(workspace, &record)
    }

    pub(super) fn append_operation_result(
        &self,
        workspace: &Workspace,
        record: &OperationRecord,
    ) -> Result<(), ServiceError> {
        self.finish_operation_in_memory(
            &record.operation_id,
            record.status.clone(),
            record.summary.clone(),
            record.artifacts.clone(),
            record.raw_output.clone(),
        )?;
        if let Err(error) = workspace.append_operation(record) {
            let message = format!(
                "operation {} execution {:?}; journal write failed: {error}; do not replay",
                record.operation_id, record.status
            );
            if let Some(op) = self
                .lock_state()?
                .operations
                .get_mut(record.operation_id.id.as_str())
            {
                op.details.output_error = Some(message.clone());
            }
            return Err(ServiceError::Worker(message));
        }
        if let Some(saved) = workspace.get_operation(&record.operation_id)? {
            if let Some(op) = self
                .lock_state()?
                .operations
                .get_mut(record.operation_id.id.as_str())
            {
                op.details = saved.details;
            }
        }
        Ok(())
    }

    pub(super) fn bind_session_worker(
        &self,
        session_id: &SessionRef,
        worker: &WorkerHandle,
        backend: &str,
    ) -> Result<(), ServiceError> {
        let mut state = self.lock_state()?;
        let session = state
            .summaries
            .get_mut(session_id.id.as_str())
            .ok_or_else(|| ServiceError::SessionNotFound(session_id.to_string()))?;
        if session
            .worker
            .as_ref()
            .is_some_and(|old| old.worker_id != worker.worker_id)
        {
            return Err(ServiceError::Worker(
                "an active session cannot replace its worker".into(),
            ));
        }
        let mut process = self.supervisor.process_instance(worker);
        if let Some(process) = &mut process {
            process.environment_id = session.control_environment.id.clone();
        }
        session.worker = Some(WorkerBinding {
            worker_id: worker.worker_id.clone(),
            environment: session.control_environment.clone(),
            process,
            backend: backend.into(),
            execution_identity: serde_json::to_value(&worker.identity)?
                .as_str()
                .unwrap_or("unknown")
                .into(),
            ownership: ProcessOwnership::Managed,
        });
        if session.kind == SessionKind::Debug {
            session.lifecycle = SessionLifecycle::Active;
        }
        session.connection = ConnectionState::Connected;
        session.updated_at = Timestamp::now();
        session.capabilities = if backend == "dbgeng" {
            vec!["exec".into(), "close".into(), "force_close".into()]
        } else if backend == "ttd" {
            vec!["status".into(), "force_close".into()]
        } else {
            vec!["status".into(), "stop".into(), "force_close".into()]
        };
        if let Some(snapshot) = self.supervisor.snapshot(worker) {
            apply_snapshot(session, snapshot);
        }
        self.persist_session(session)
    }

    pub(super) fn set_session_terminal(
        &self,
        session_id: &SessionRef,
        failed: bool,
        cleanup_complete: bool,
        error: Option<String>,
    ) -> Result<(), ServiceError> {
        let mut state = self.lock_state()?;
        if let Some(session) = state.summaries.get_mut(session_id.id.as_str()) {
            session.lifecycle = if failed {
                SessionLifecycle::Failed
            } else {
                SessionLifecycle::Closed
            };
            session.connection = ConnectionState::Disconnected;
            if session.target_state.state != TargetState::Offline {
                if session.target_state.state != TargetState::Unknown {
                    session.target_state.last_known = Some(session.target_state.state);
                }
                session.target_state.state = TargetState::Unknown;
                session.target_state.source = "worker_disconnected".into();
                session.target_state.observed_at = Timestamp::now();
            }
            session.updated_at = Timestamp::now();
            session.cleanup.complete = cleanup_complete;
            session.cleanup.errors = if cleanup_complete {
                vec![]
            } else {
                error.clone().into_iter().collect()
            };
            session.error = error;
            self.persist_session(session)?;
        }
        Ok(())
    }

    pub(super) fn persist_session(&self, session: &Session) -> Result<(), ServiceError> {
        let workspace = Workspace::open(&session.workspace.root)?;
        let dir = match session.kind {
            SessionKind::Debug => workspace.ensure_session_artifact_dir(&session.session_id.id)?,
            SessionKind::Recording => {
                workspace.ensure_recording_artifact_dir(&session.session_id.id)?
            }
        };
        write_atomic_bytes(
            &dir.join("session.json"),
            &serde_json::to_vec_pretty(session)?,
        )?;
        Ok(())
    }

    fn record_worker_exit(&self, session_id: &SessionRef) -> Result<(), ServiceError> {
        let (session, operations) = {
            let state = self.lock_state()?;
            (
                state.summaries.get(session_id.id.as_str()).cloned(),
                state
                    .operations
                    .values()
                    .filter(|op| {
                        op.session_id.as_ref() == Some(session_id)
                            && op.status == OperationStatus::Running
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        if let Some(session) = session {
            let workspace = Workspace::open(&session.workspace.root)?;
            let directory = if session.kind == SessionKind::Debug {
                workspace.ensure_session_artifact_dir(&session_id.id)?
            } else {
                workspace.ensure_recording_artifact_dir(&session_id.id)?
            };
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join("session-events.jsonl"))?;
            writeln!(
                file,
                "{}",
                json!({"schema_version":1,"session_id":session_id,"event":"worker_exited","observed_at":Timestamp::now(),"source":"process_monitor","cleanup":session.cleanup})
            )?;
            for operation in operations {
                self.finish_operation_in_memory(
                    &operation.operation_id,
                    OperationStatus::Unknown,
                    "worker exited; execution outcome unknown".into(),
                    operation.artifacts.clone(),
                    operation.raw_output.clone(),
                )?;
                workspace.append_operation(&OperationRecord {
                    details: operation.details,
                    operation_id: operation.operation_id,
                    adapter_id: "service".into(),
                    capability: operation.capability,
                    status: OperationStatus::Unknown,
                    created_at: operation.created_at,
                    summary: "worker exited; execution outcome unknown".into(),
                    artifacts: operation.artifacts,
                    raw_output: operation.raw_output,
                })?;
            }
        }
        Ok(())
    }

    pub(super) fn session_get(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params: SessionParams = parse_params(params)?;
        let worker = {
            let state = self.lock_state()?;
            let terminal = state
                .summaries
                .get(params.session_id.id.as_str())
                .is_some_and(|s| s.lifecycle.is_terminal());
            if terminal {
                None
            } else {
                state
                    .sessions
                    .get(params.session_id.id.as_str())
                    .map(|s| s.worker.clone())
                    .or_else(|| {
                        state
                            .remote_recordings
                            .get(params.session_id.id.as_str())
                            .map(|s| s.worker.clone())
                    })
            }
        };
        if let Some(worker) = worker {
            if self.supervisor.is_alive(&worker)? == Some(false) {
                self.mark_debug_session_error(&params.session_id)?;
                let cleanup = self.supervisor.kill_worker(&worker);
                self.set_session_terminal(
                    &params.session_id,
                    true,
                    cleanup.is_ok(),
                    Some(format!("worker exited; cleanup: {cleanup:?}")),
                )?;
                let recording = self
                    .lock_state()?
                    .remote_recordings
                    .get(params.session_id.id.as_str())
                    .cloned();
                if let Some(recording) = recording {
                    if let Some(remote) = self
                        .lock_state()?
                        .remote_recordings
                        .get_mut(params.session_id.id.as_str())
                    {
                        remote.terminal = true;
                    }
                    if cleanup.is_ok() {
                        self.release_recording_slot(&recording)?;
                    }
                }
                self.record_worker_exit(&params.session_id)?;
            } else {
                let snapshot = self.supervisor.snapshot(&worker);
                let mut state = self.lock_state()?;
                if let Some(session) = state.summaries.get_mut(params.session_id.id.as_str()) {
                    let process_exit_observed =
                        session.target.process.as_ref().is_some_and(|process| {
                            crate::process_identity::is_alive(process).ok() == Some(false)
                        });
                    if let Some(mut snapshot) = snapshot {
                        if process_exit_observed {
                            snapshot.target_state = TargetState::Exited;
                        }
                        apply_snapshot(session, snapshot);
                    }
                    if process_exit_observed {
                        session.target_state.last_known = Some(session.target_state.state);
                        session.target_state.state = TargetState::Exited;
                        session.target_state.observed_at = Timestamp::now();
                        session.target_state.source = "process_monitor".into();
                        session.updated_at = Timestamp::now();
                    }
                    self.persist_session(session)?;
                }
            }
        }
        let state = self.lock_state()?;
        let session = state
            .summaries
            .get(params.session_id.id.as_str())
            .ok_or_else(|| ServiceError::SessionNotFound(params.session_id.to_string()))?;
        Ok(serde_json::to_value(session)?)
    }
}

fn apply_snapshot(session: &mut Session, mut snapshot: dbgatlas_worker_protocol::WorkerSnapshot) {
    if session.lifecycle.is_terminal() {
        return;
    }
    if let Some(process) = &mut snapshot.target_process {
        process.environment_id = session.control_environment.id.clone();
        if session
            .target
            .process
            .as_ref()
            .is_some_and(|old| !old.same_instance(process))
        {
            session.target.revision += 1;
            // A target selected by an arbitrary debugger command is not thereby owned.
            session.target.created_by_session = false;
        }
        session.target.kind = "process".into();
        session.target.process = Some(process.clone());
    } else if snapshot.target_state == TargetState::Unknown && session.target.process.is_some() {
        session.target.revision += 1;
        session.target.process = None;
        session.target.created_by_session = false;
    }
    session.target_state = StateObservation {
        state: snapshot.target_state,
        observed_at: snapshot.observed_at,
        source: "backend_api".into(),
        last_known: Some(session.target_state.state),
    };
    session.context = snapshot.context;
    session.updated_at = Timestamp::now();
}
