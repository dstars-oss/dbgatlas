use super::*;
use std::sync::Condvar;

pub(super) const DEFAULT_WAIT_MS: u64 = 1000;
pub(super) const MAX_WAIT_MS: u64 = 30_000;

#[derive(Clone)]
pub(super) struct Scope {
    pub operation_id: OperationRef,
    pub session_id: SessionRef,
}

pub(super) struct Job {
    pub scope: Scope,
    pub action: String,
    pub workspace: PathBuf,
    pub input: Value,
    pub related: Option<OperationRef>,
    pub state: Mutex<JobState>,
    pub changed: Condvar,
}

#[derive(Default)]
pub(super) struct JobState {
    pub started: bool,
    pub finished: bool,
    pub cancel_requested: bool,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Default)]
pub(super) struct Registry {
    pub jobs: HashMap<String, Arc<Job>>,
    pub busy: HashMap<String, OperationRef>,
    pub closing: HashMap<String, OperationRef>,
    pub keys: HashMap<(String, String), (Value, OperationRef)>,
    pub observations: HashMap<String, (Value, u64)>,
}

#[derive(Default)]
pub(super) struct Coordinator {
    pub registry: Mutex<Registry>,
    pub changed: Condvar,
    pub cursors: Mutex<super::output_read::Cursors>,
}

pub(super) fn wait_budget(params: &Value, default: u64) -> Result<u64, ServiceError> {
    if params.get("timeout_ms").is_some_and(|v| !v.is_null()) {
        return Err(ServiceError::Rpc("timeout_ms is no longer an execution deadline; use wait_ms for bounded observation, execution.cancel to cancel, or session.kill explicitly".into()));
    }
    match params.get("wait_ms") {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value.as_u64().filter(|v| *v <= MAX_WAIT_MS).ok_or_else(|| {
            ServiceError::Rpc("wait_ms must be an integer from 0 through 30000".into())
        }),
    }
}

pub(super) fn terminal(status: &OperationStatus) -> bool {
    !matches!(status, OperationStatus::Pending | OperationStatus::Running)
}

pub(super) fn action_name(method: &str) -> &str {
    match method {
        "debug.session.create" => "session.open",
        "debug.session.close" => "session.close",
        "debug.session.kill" => "session.kill",
        "debug.eval"
        | "debug.modules"
        | "debug.threads"
        | "debug.stack"
        | "debug.eval_steps.step" => "session.exec",
        "operation.cancel" => "execution.cancel",
        other => other,
    }
}

impl ServiceHost {
    pub(super) fn batch_stop_requested(&self, id: &OperationRef, sid: &SessionRef) -> bool {
        let registry = self.executions.registry.lock().unwrap();
        registry.closing.contains_key(sid.id.as_str())
            || registry
                .jobs
                .get(id.id.as_str())
                .is_some_and(|job| job.state.lock().unwrap().cancel_requested)
    }

    pub(super) fn action_operation_id(&self) -> OperationRef {
        self.execution_scope
            .as_ref()
            .map(|s| s.operation_id.clone())
            .unwrap_or_else(next_operation_ref)
    }

    pub(super) fn execution_job(&self, id: &OperationRef) -> Result<Arc<Job>, ServiceError> {
        self.executions
            .registry
            .lock()
            .unwrap()
            .jobs
            .get(id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::OperationNotFound(id.to_string()))
    }

    /// All public debug actions, including compatibility methods, enter here.
    /// Admission and durable acceptance happen before returning any execution ID.
    pub(super) fn submit_execution(
        &self,
        method: &str,
        params: Option<Value>,
        compatibility: bool,
    ) -> Result<Value, ServiceError> {
        let mut params = params.unwrap_or_else(|| json!({}));
        let wait_ms = wait_budget(&params, DEFAULT_WAIT_MS)?;
        let action = action_name(method).to_string();
        let legacy = match method {
            "session.open" => "debug.session.create",
            "session.exec" => "debug.eval",
            "session.close" => "debug.session.close",
            "session.kill" => "debug.session.kill",
            "execution.cancel" => "operation.cancel",
            other => other,
        }
        .to_string();
        if legacy == "debug.session.create" {
            if params.get("backend").is_some_and(|v| v != "dbgeng") {
                return Err(ServiceError::Rpc(
                    "unsupported backend; session.open currently supports dbgeng".into(),
                ));
            }
            self.resolve_input_artifact(&mut params)?;
            parse_debug_session_create_params(Some(params.clone()))?
                .target
                .validate()?;
        }
        let related = if legacy == "operation.cancel" {
            let id: OperationRef = serde_json::from_value(
                params
                    .get("execution_id")
                    .or_else(|| params.get("operation_id"))
                    .cloned()
                    .ok_or_else(|| ServiceError::Rpc("execution_id is required".into()))?,
            )?;
            params["operation_id"] = serde_json::to_value(&id)?;
            Some(id)
        } else {
            None
        };
        let sid = if legacy == "debug.session.create" {
            next_session_ref()
        } else if let Some(id) = &related {
            self.lock_state()?
                .operations
                .get(id.id.as_str())
                .and_then(|op| op.session_id.clone())
                .ok_or_else(|| ServiceError::OperationNotFound(id.to_string()))?
        } else {
            serde_json::from_value(
                params
                    .get("session_id")
                    .cloned()
                    .ok_or_else(|| ServiceError::Rpc("session_id is required".into()))?,
            )?
        };
        if let Some(instance) = params.get("expected_service_instance") {
            if instance.as_str() != Some(&self.service_instance) {
                return Err(ServiceError::Rpc(
                    "service_instance_changed; do not replay the previous request".into(),
                ));
            }
        }
        let key = params
            .get("client_request_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if params.get("client_request_id").is_some()
            && (key.as_ref().is_none_or(|k| k.is_empty() || k.len() > 256)
                || params.get("expected_service_instance").is_none())
        {
            return Err(ServiceError::Rpc("client_request_id requires a nonempty key (at most 256 bytes) and expected_service_instance".into()));
        }
        if key.is_some() && legacy != "debug.eval" {
            return Err(ServiceError::Rpc(
                "client_request_id is supported for session.exec only".into(),
            ));
        }
        // Budgets and transport retry keys are not execution inputs.
        if let Some(object) = params.as_object_mut() {
            for field in [
                "wait_ms",
                "timeout_ms",
                "client_request_id",
                "expected_service_instance",
                "backend",
                "execution_id",
            ] {
                object.remove(field);
            }
        }
        if legacy == "debug.eval" {
            let input: DebugEvalParams = parse_params(Some(params.clone()))?;
            EvalDebugCommand {
                session_id: sid.clone(),
                command: input.command,
                timeout_ms: None,
            }
            .validate()?;
        }
        let mut registry = self.executions.registry.lock().unwrap();
        if let Some(key) = key.as_ref() {
            if let Some((input, id)) = registry.keys.get(&(sid.to_string(), key.clone())) {
                if input != &params {
                    return Err(ServiceError::Rpc(
                        "client_request_id conflicts with a different execution input".into(),
                    ));
                }
                let job = registry.jobs[id.id.as_str()].clone();
                drop(registry);
                return self.wait_execution_response(job, wait_ms, compatibility);
            }
        }
        if action == "session.close" {
            if let Some(id) = registry.closing.get(sid.id.as_str()) {
                let job = registry.jobs[id.id.as_str()].clone();
                drop(registry);
                return self.wait_execution_response(job, wait_ms, compatibility);
            }
        }
        let control = matches!(
            action.as_str(),
            "session.interrupt" | "execution.cancel" | "session.close" | "session.kill"
        );
        let summary = if legacy == "debug.session.create" {
            None
        } else {
            Some(
                self.lock_state()?
                    .summaries
                    .get(sid.id.as_str())
                    .cloned()
                    .ok_or_else(|| ServiceError::SessionNotFound(sid.to_string()))?,
            )
        };
        if !control && legacy != "debug.session.create" {
            if registry.closing.contains_key(sid.id.as_str()) {
                return Err(ServiceError::SessionBusy(format!("session_closing: {sid}")));
            }
            if registry.busy.contains_key(sid.id.as_str()) {
                return Err(ServiceError::SessionBusy(sid.to_string()));
            }
            let session = self.reusable_debug_session(&sid)?;
            if self
                .supervisor
                .snapshot(&session.worker)
                .is_some_and(|s| s.context["engine"]["state"] == "waiting_for_event")
            {
                return Err(ServiceError::Rpc(
                    "target_running; observe or interrupt before executing another command".into(),
                ));
            }
        }
        if let Some(summary) = &summary {
            if summary.lifecycle.is_terminal()
                && !matches!(
                    action.as_str(),
                    "execution.cancel" | "session.close" | "session.kill"
                )
            {
                return Err(ServiceError::SessionAlreadyTerminal(sid.to_string()));
            }
        }
        if registry
            .jobs
            .values()
            .filter(|j| !j.state.lock().unwrap().finished)
            .count()
            >= 128
        {
            return Err(ServiceError::Rpc("too many active executions".into()));
        }
        let id = next_operation_ref();
        let workspace = if legacy == "debug.session.create" {
            let p = parse_debug_session_create_params(Some(params.clone()))?;
            ensure_project_workspace(&p.project_root)?
        } else {
            Workspace::open(&summary.as_ref().unwrap().workspace.root)?
        };
        if legacy == "debug.session.create" {
            self.open_session_record(
                &sid,
                &workspace,
                dbgatlas_model::SessionKind::Debug,
                params["target"].clone(),
                &id,
                &legacy,
                params.clone(),
            )?;
            if let Some(input) = params.get("input_artifact_ref") {
                let reference = serde_json::from_value(input.clone())?;
                let mut state = self.lock_state()?;
                let session = state.summaries.get_mut(sid.id.as_str()).unwrap();
                session.input_artifacts.push(reference);
                self.persist_session(session)?;
            }
        } else {
            self.begin_operation_record(&workspace, &id, &sid, &legacy, params.clone())?;
        }
        if let Some(op) = self.lock_state()?.operations.get_mut(id.id.as_str()) {
            op.status = OperationStatus::Pending;
            op.details.related_operation_id = related.clone();
        }
        let job = Arc::new(Job {
            scope: Scope {
                operation_id: id.clone(),
                session_id: sid.clone(),
            },
            action,
            workspace: workspace.root().into(),
            input: params.clone(),
            related,
            state: Mutex::new(JobState::default()),
            changed: Condvar::new(),
        });
        registry.jobs.insert(id.to_string(), job.clone());
        if !control {
            registry.busy.insert(sid.to_string(), id.clone());
        }
        if matches!(job.action.as_str(), "session.close" | "session.kill") {
            registry.closing.insert(sid.to_string(), id.clone());
        }
        if let Some(key) = key {
            registry
                .keys
                .insert((sid.to_string(), key), (params.clone(), id.clone()));
        }
        let predecessor = if matches!(job.action.as_str(), "session.close" | "session.kill") {
            registry
                .busy
                .get(sid.id.as_str())
                .and_then(|id| registry.jobs.get(id.id.as_str()))
                .cloned()
        } else {
            None
        };
        drop(registry);
        let mut host = self.clone();
        host.execution_scope = Some(job.scope.clone());
        let task = job.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("execution-{}", id.id))
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    task.state.lock().unwrap().started = true;
                    task.changed.notify_all();
                    if let Some(op) = host
                        .lock_state()?
                        .operations
                        .get_mut(task.scope.operation_id.id.as_str())
                    {
                        op.status = OperationStatus::Running;
                    }
                    if let Some(previous) = predecessor {
                        if task.action == "session.close" || previous.action == "session.open" {
                            host.await_predecessor(&previous, task.action == "session.close")?;
                        }
                    }
                    host.run_execution_action(&legacy, params)
                }))
                .unwrap_or_else(|_| {
                    let _ = host.finish_operation_in_memory(
                        &task.scope.operation_id,
                        OperationStatus::Unknown,
                        "execution task panicked; outcome unknown; do not replay".into(),
                        vec![],
                        None,
                    );
                    Err(ServiceError::Worker(
                        "execution task panicked; outcome unknown; do not replay".into(),
                    ))
                });
                host.complete_execution(&task, result);
            });
        if let Err(error) = spawned {
            self.complete_execution(&job, Err(error.into()));
        }
        self.wait_execution_response(job, wait_ms, compatibility)
    }

    fn await_predecessor(&self, previous: &Arc<Job>, cancel: bool) -> Result<(), ServiceError> {
        loop {
            let state = previous.state.lock().unwrap();
            if state.finished {
                return Ok(());
            }
            drop(state);
            if cancel {
                let worker = self
                    .lock_state()?
                    .sessions
                    .get(previous.scope.session_id.id.as_str())
                    .map(|s| s.worker.clone());
                if let Some(worker) = worker {
                    let operation = self
                        .supervisor
                        .snapshot(&worker)
                        .and_then(|s| {
                            serde_json::from_value(
                                s.context["engine"]["active_operation_id"].clone(),
                            )
                            .ok()
                        })
                        .unwrap_or_else(|| previous.scope.operation_id.clone());
                    let _ = self.supervisor.cancel_worker_operation(
                        &worker,
                        &previous.scope.session_id,
                        &operation,
                    );
                }
            }
            let state = previous.state.lock().unwrap();
            let _ = previous
                .changed
                .wait_timeout(state, Duration::from_millis(50))
                .unwrap();
        }
    }

    fn run_execution_action(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
        match method {
            "debug.session.create" => self.debug_session_create(Some(params)),
            "debug.eval" => self.debug_eval(Some(params)),
            "debug.eval_steps" => self.debug_eval_steps(Some(params)),
            "debug.modules" => self.debug_builtin_eval(Some(params), "debug.modules", "lm"),
            "debug.threads" => self.debug_builtin_eval(Some(params), "debug.threads", "~"),
            "debug.stack" => self.debug_builtin_eval(Some(params), "debug.stack", "k"),
            "debug.add_symbols" => self.debug_add_symbols(Some(params)),
            "debug.read_memory" => self.debug_read_memory(Some(params)),
            "session.interrupt" => self.session_interrupt(Some(params)),
            "operation.cancel" => {
                let p: OperationGetParams = parse_params(Some(params.clone()))?;
                let op = self
                    .lock_state()?
                    .operations
                    .get(p.operation_id.id.as_str())
                    .cloned()
                    .ok_or_else(|| ServiceError::OperationNotFound(p.operation_id.to_string()))?;
                if terminal(&op.status) {
                    self.finish_operation_in_memory(
                        &self.action_operation_id(),
                        OperationStatus::Success,
                        "already_finished".into(),
                        vec![],
                        None,
                    )?;
                    Ok(json!({"accepted":false,"code":"already_finished"}))
                } else {
                    self.operation_cancel(Some(params))
                }
            }
            "debug.session.close" | "debug.session.kill" => {
                let sid = &self.execution_scope.as_ref().unwrap().session_id;
                if self
                    .lock_state()?
                    .summaries
                    .get(sid.id.as_str())
                    .is_some_and(|s| s.lifecycle.is_terminal() && s.cleanup.complete)
                {
                    self.finish_operation_in_memory(
                        &self.action_operation_id(),
                        OperationStatus::Success,
                        "already_closed".into(),
                        vec![],
                        None,
                    )?;
                    Ok(json!({"code":"already_closed"}))
                } else if method == "debug.session.close" {
                    self.debug_session_close(Some(params))
                } else {
                    self.debug_session_kill(Some(params))
                }
            }
            _ => Err(ServiceError::Rpc(format!(
                "unsupported execution action: {method}"
            ))),
        }
    }

    fn complete_execution(&self, job: &Arc<Job>, result: Result<Value, ServiceError>) {
        let error = result.as_ref().err().map(ToString::to_string);
        if let Ok(mut state) = self.lock_state() {
            if let Some(op) = state.operations.get_mut(job.scope.operation_id.id.as_str()) {
                if !terminal(&op.status) {
                    op.status = if result.is_ok() {
                        OperationStatus::Success
                    } else {
                        OperationStatus::Failed
                    };
                }
                op.updated_at = Timestamp::now();
                op.details.update_seq += 1;
                op.details.related_operation_id = job.related.clone();
            }
        }
        if let Ok(workspace) = Workspace::open(&job.workspace) {
            if let Err(error) = self.archive_execution_streams(job, &workspace) {
                if let Ok(mut state) = self.lock_state() {
                    if let Some(op) = state.operations.get_mut(job.scope.operation_id.id.as_str()) {
                        op.details.output_error = Some(error.to_string());
                    }
                }
            }
            let op = self.lock_state().ok().and_then(|s| {
                s.operations
                    .get(job.scope.operation_id.id.as_str())
                    .cloned()
            });
            if let Some(op) = op {
                let persisted = self.append_operation_result(
                    &workspace,
                    &OperationRecord {
                        details: op.details,
                        operation_id: op.operation_id,
                        adapter_id: "service".into(),
                        capability: op.capability,
                        status: op.status,
                        created_at: op.created_at,
                        summary: error.clone().unwrap_or(op.summary),
                        artifacts: op.artifacts,
                        raw_output: op.raw_output,
                    },
                );
                if let Err(error) = persisted {
                    append_service_diagnostic_log(&format!(
                        "execution finalization failed: {error}"
                    ));
                }
            }
        }
        {
            let mut registry = self.executions.registry.lock().unwrap();
            if registry.busy.get(job.scope.session_id.id.as_str()) == Some(&job.scope.operation_id)
            {
                registry.busy.remove(job.scope.session_id.id.as_str());
            }
        }
        {
            let mut state = job.state.lock().unwrap();
            state.finished = true;
            state.error = error;
            state.result = result.ok();
        }
        job.changed.notify_all();
        self.executions.changed.notify_all();
    }

    fn archive_execution_streams(
        &self,
        job: &Job,
        workspace: &Workspace,
    ) -> Result<(), ServiceError> {
        let dir = workspace.ensure_session_artifact_dir(&job.scope.session_id.id)?;
        let op = self
            .lock_state()?
            .operations
            .get(job.scope.operation_id.id.as_str())
            .cloned()
            .unwrap();
        if job.action == "session.exec" && op.raw_output.is_none() {
            let path = dir
                .join("raw")
                .join(format!("{}.txt.part", job.scope.operation_id.id));
            if path.is_file() {
                let artifact = next_artifact_ref();
                workspace.register_artifact(&ArtifactMetadata {
                    artifact_id: artifact.clone(),
                    kind: "debug.raw_output".into(),
                    relative_path: path.strip_prefix(workspace.root()).unwrap().into(),
                    created_at: Timestamp::now(),
                    operation_id: Some(job.scope.operation_id.clone()),
                    producer_session_id: Some(job.scope.session_id.clone()),
                    producer_execution_id: Some(job.scope.operation_id.clone()),
                    byte_len: Some(fs::metadata(&path)?.len()),
                    description: Some(
                        "Retained output of an interrupted or failed execution".into(),
                    ),
                })?;
                let mut state = self.lock_state()?;
                let op = state
                    .operations
                    .get_mut(job.scope.operation_id.id.as_str())
                    .unwrap();
                op.raw_output = Some(artifact.clone());
                op.artifacts.push(artifact);
            }
        }
        if matches!(job.action.as_str(), "session.close" | "session.kill") {
            let summary = self
                .lock_state()?
                .summaries
                .get(job.scope.session_id.id.as_str())
                .cloned()
                .unwrap();
            if summary.cleanup.complete && summary.lifecycle.is_terminal() {
                let existing = workspace.list_artifacts()?;
                let mut references = Vec::new();
                for name in [
                    "session-output.log",
                    "engine-events.jsonl",
                    "session-events.jsonl",
                ] {
                    let path = dir.join(name);
                    if !path.is_file() {
                        continue;
                    }
                    let relative = path.strip_prefix(workspace.root()).unwrap().to_path_buf();
                    if existing.iter().any(|a| a.relative_path == relative) {
                        continue;
                    }
                    let artifact = next_artifact_ref();
                    workspace.register_artifact(&ArtifactMetadata {
                        artifact_id: artifact.clone(),
                        kind: "debug.session_stream".into(),
                        relative_path: relative,
                        created_at: Timestamp::now(),
                        operation_id: None,
                        producer_session_id: Some(job.scope.session_id.clone()),
                        producer_execution_id: None,
                        byte_len: Some(fs::metadata(&path)?.len()),
                        description: Some(
                            "Asynchronous session output; not attributed to an execution".into(),
                        ),
                    })?;
                    references.push(artifact);
                }
                let mut state = self.lock_state()?;
                let session = state
                    .summaries
                    .get_mut(job.scope.session_id.id.as_str())
                    .unwrap();
                session.artifacts.extend(references);
                self.persist_session(session)?;
            }
        }
        Ok(())
    }

    fn wait_execution_response(
        &self,
        job: Arc<Job>,
        wait_ms: u64,
        compatibility: bool,
    ) -> Result<Value, ServiceError> {
        let state = job.state.lock().unwrap();
        let (state, _) = job
            .changed
            .wait_timeout_while(state, Duration::from_millis(wait_ms), |s| !s.finished)
            .unwrap();
        let finished = state.finished;
        let legacy = state.result.clone();
        let error = state.error.clone();
        drop(state);
        let execution = self.execution_value(&job.scope.operation_id)?;
        let session = self.observed_session(&job.scope.session_id)?;
        if compatibility && finished {
            if let Some(mut legacy) = legacy {
                legacy["execution"] = execution;
                legacy["session"] = session;
                legacy["wait_expired"] = json!(false);
                return Ok(legacy);
            }
            return Err(ServiceError::Worker(
                error.unwrap_or_else(|| "execution failed".into()),
            ));
        }
        let mut response = json!({"execution_id":job.scope.operation_id,"session_id":job.scope.session_id,"service_instance":self.service_instance,"execution":execution,"session":session,"wait_expired":!finished,"output":{"preview":legacy.as_ref().and_then(|r|r.get("result")).cloned().unwrap_or(Value::Null),"cursor":Value::Null,"complete":execution["output_complete"]},"output_error":execution["output_error"]});
        if let Some(related) = &job.related {
            response["control_execution"] = execution;
            response["execution"] = self.execution_value(related)?;
        }
        if let Some(legacy) = legacy {
            for field in [
                "accepted",
                "code",
                "epoch",
                "native_hresult",
                "cancellation_requested",
            ] {
                if let Some(v) = legacy.get(field) {
                    response[field] = v.clone();
                }
            }
        }
        if compatibility {
            response["operation_id"] = json!(job.scope.operation_id);
            response["operation_status"] = response["execution"]["status"].clone();
        }
        Ok(response)
    }

    pub(super) fn execution_value(&self, id: &OperationRef) -> Result<Value, ServiceError> {
        if let Err(error) = self.capture_execution_start(id) {
            append_service_diagnostic_log(&format!(
                "execution context observation failed: {error}"
            ));
        }
        let op = self
            .lock_state()?
            .operations
            .get(id.id.as_str())
            .cloned()
            .ok_or_else(|| ServiceError::OperationNotFound(id.to_string()))?;
        let job = self
            .executions
            .registry
            .lock()
            .unwrap()
            .jobs
            .get(id.id.as_str())
            .cloned();
        let (action, input, related, finished, phase, batch_result) = if let Some(job) = job {
            let state = job.state.lock().unwrap();
            let batch=state.result.as_ref().filter(|v|v.get("steps").is_some()).map(|v|json!({"stop_reason":v["stop_reason"],"unexecuted_steps":v["unexecuted_steps"],"steps":v["steps"].as_array().map(|steps|steps.iter().map(|s|json!({"execution_id":s["operation_id"],"step_index":s["step_index"],"status":s["operation_status"]})).collect::<Vec<_>>())}));
            (
                job.action.clone(),
                job.input.clone(),
                job.related.clone(),
                state.finished,
                if state.finished {
                    2
                } else if state.started {
                    1
                } else {
                    0
                },
                batch,
            )
        } else {
            (
                action_name(&op.capability).into(),
                op.details.input.clone().unwrap_or(Value::Null),
                op.details.related_operation_id.clone(),
                terminal(&op.status),
                2,
                None,
            )
        };
        let error = self
            .execution_job(id)
            .ok()
            .and_then(|job| job.state.lock().unwrap().error.clone());
        let op = if finished {
            self.lock_state()?
                .operations
                .get(id.id.as_str())
                .cloned()
                .unwrap_or(op)
        } else {
            op
        };
        Ok(
            json!({"execution_id":id,"batch_result":batch_result,"error":error,"finalized":finished,"session_id":op.session_id.or(op.details.session_id),"action":action,"input":input,"status":op.status,"sequence":op.details.update_seq*4+phase,"summary":op.summary,"execution_context":op.details.execution_context,"artifact_refs":op.artifacts,"raw_output_ref":op.raw_output,"output_error":op.details.output_error,"output_complete":finished && op.status!=OperationStatus::Unknown && op.details.output_error.is_none(),"related_execution_id":related,"parent_execution_id":op.details.input.as_ref().and_then(|v|v.get("batch_operation_id")),"created_at":op.created_at,"updated_at":op.updated_at}),
        )
    }

    pub(super) fn execution_get(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params = params.unwrap_or_default();
        let budget = wait_budget(&params, 0)?;
        let id: OperationRef = serde_json::from_value(params["execution_id"].clone())?;
        let after = params.get("after_seq").and_then(Value::as_u64);
        let deadline = Instant::now() + Duration::from_millis(budget);
        loop {
            let value = self.execution_value(&id)?;
            if after
                .map(|seq| value["sequence"].as_u64().unwrap_or(0) > seq)
                .unwrap_or(budget == 0)
                || value["finalized"] == true
                || Instant::now() >= deadline
            {
                return Ok(value);
            }
            if let Ok(job) = self.execution_job(&id) {
                let state = job.state.lock().unwrap();
                let _ = job
                    .changed
                    .wait_timeout(
                        state,
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(25)),
                    )
                    .unwrap();
            } else {
                return Ok(value);
            }
        }
    }

    pub(super) fn observed_session(&self, sid: &SessionRef) -> Result<Value, ServiceError> {
        let mut value = self.session_get(Some(json!({"session_id":sid})))?;
        let mut registry = self.executions.registry.lock().unwrap();
        let active = registry.busy.get(sid.id.as_str()).cloned();
        let mut engine = value["context"]["engine"].clone();
        if !engine.is_object() {
            engine = json!({"state":"unavailable"});
        }
        engine["run_epoch"] = engine["epoch"].take();
        engine["source"] = json!("backend_observation");
        engine["observed_at"] = value["target_state"]["observed_at"].clone();
        if value["connection"] != "connected" {
            engine["state"] = json!("unavailable");
        }
        value["engine"] = engine;
        value["active_execution_id"] = json!(active);
        value["last_execution_id"] = value["last_operation_id"].clone();
        if registry.closing.contains_key(sid.id.as_str())
            && value["lifecycle"] != "closed"
            && value["lifecycle"] != "failed"
        {
            value["lifecycle"] = json!("closing");
        }
        let signature = json!({"lifecycle":value["lifecycle"],"connection":value["connection"],"target":value["target"],"target_state":value["target_state"]["state"],"engine_state":value["engine"]["state"],"epoch":value["engine"]["run_epoch"],"active":value["active_execution_id"],"last":value["last_execution_id"],"event":value["context"]["last_event"],"input_required":value["context"]["input_required"],"cleanup":value["cleanup"]});
        let entry = registry
            .observations
            .entry(sid.to_string())
            .or_insert((Value::Null, 0));
        if entry.0 != signature {
            entry.0 = signature;
            entry.1 += 1;
        }
        value["sequence"] = json!(entry.1);
        Ok(value)
    }

    pub(super) fn session_observe(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params = params.unwrap_or_default();
        let budget = wait_budget(&params, 0)?;
        let sid: SessionRef = serde_json::from_value(params["session_id"].clone())?;
        let after = params.get("after_seq").and_then(Value::as_u64);
        let deadline = Instant::now() + Duration::from_millis(budget);
        loop {
            let value = self.observed_session(&sid)?;
            if after.is_none_or(|seq| value["sequence"].as_u64().unwrap_or(0) > seq)
                || Instant::now() >= deadline
                || value["lifecycle"] == "closed"
                || value["lifecycle"] == "failed"
            {
                return Ok(value);
            }
            let registry = self.executions.registry.lock().unwrap();
            let _ = self
                .executions
                .changed
                .wait_timeout(
                    registry,
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(25)),
                )
                .unwrap();
        }
    }

    fn resolve_input_artifact(&self, params: &mut Value) -> Result<(), ServiceError> {
        let Some(reference) = params["target"].get("artifact_ref").cloned() else {
            return Ok(());
        };
        if params["target"].get("path").is_some() {
            return Err(ServiceError::Rpc(
                "file target requires exactly one of path or artifact_ref".into(),
            ));
        }
        if params["target"]["kind"] != "file" {
            return Err(ServiceError::Rpc(
                "artifact_ref requires a file target".into(),
            ));
        }
        let workspace = ensure_project_workspace(Path::new(
            params["project_root"]
                .as_str()
                .ok_or_else(|| ServiceError::Rpc("project_root is required".into()))?,
        ))?;
        let reference: ArtifactRef = serde_json::from_value(reference)?;
        let artifact = workspace
            .get_artifact(&reference)?
            .ok_or_else(|| ServiceError::Rpc("artifact not found in this workspace".into()))?;
        let path = workspace.resolve_artifact_relative_path(&artifact.relative_path)?;
        if !path.is_file() {
            return Err(ServiceError::Rpc(
                "input artifact is not a readable file".into(),
            ));
        }
        params["target"]
            .as_object_mut()
            .unwrap()
            .remove("artifact_ref");
        params["target"]["path"] = json!(path);
        params["input_artifact_ref"] = json!(reference);
        Ok(())
    }

    pub(super) fn execution_help(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params = params.unwrap_or_default();
        if params.get("session_id").is_some() && params.get("backend").is_some() {
            return Err(ServiceError::Rpc("choose session_id or backend".into()));
        }
        let topic = params["topic"].as_str().unwrap_or("overview");
        let text = match topic {
            "overview" => {
                "Use session.open, session.exec and session.get. One native command is passed through unchanged. Execution success means the backend call returned successfully, not that the target stopped or the analysis goal was met."
            }
            "execution" => {
                "wait_ms defaults to 1000 for actions and is limited to 30000. Expiry returns a pending/running execution without canceling it. Use execution.get; use client_request_id with expected_service_instance for safe same-instance retry. Query commands include lm, ~ and k; continue with g, then observe session.get before another command."
            }
            "output" => {
                "execution.read reads command output; session.read reads asynchronous output and event JSONL. Keep next_cursor and continue while has_more. complete=false can mean running, finalizing or an output gap. Cursors are resource- and service-instance-bound. Binary data is referenced as artifacts."
            }
            "control" => {
                "execution.cancel requests PASSIVE cancellation of one active command. session.interrupt requests ACTIVE interruption of the observed run_epoch. Neither acceptance confirms a stop. session.close coordinates cancellation and detach, preserving targets; session.kill explicitly terminates the worker and only verified owned resources."
            }
            _ => return Err(ServiceError::Rpc("unknown help topic".into())),
        };
        let session = if let Some(id) = params.get("session_id") {
            Some(self.observed_session(&serde_json::from_value(id.clone())?)?)
        } else {
            None
        };
        if params
            .get("backend")
            .is_some_and(|backend| backend != "dbgeng")
        {
            return Err(ServiceError::Rpc("unsupported backend".into()));
        }
        Ok(json!({"backend":"dbgeng","topic":topic,"text":text,"session":session}))
    }

    pub(super) fn capture_execution_start(&self, id: &OperationRef) -> Result<(), ServiceError> {
        let (operation, session, summary) = {
            let state = self.lock_state()?;
            let Some(op) = state
                .operations
                .get(id.id.as_str())
                .filter(|op| op.details.execution_context.is_none())
            else {
                return Ok(());
            };
            let Some(sid) = op.session_id.as_ref() else {
                return Ok(());
            };
            let Some(session) = state.sessions.get(sid.id.as_str()) else {
                return Ok(());
            };
            let Some(summary) = state.summaries.get(sid.id.as_str()) else {
                return Ok(());
            };
            (op.clone(), session.clone(), summary.clone())
        };
        let start = self
            .supervisor
            .snapshot(&session.worker)
            .map(|s| s.context["execution_start"].clone());
        let mut target = summary.target;
        let context = if let Some(start) = start.filter(|s| s["operation_id"] == json!(id)) {
            if let Ok(process) = serde_json::from_value(start["target_process"].clone()) {
                target.process = process;
            }
            let mut context = start["context"].clone();
            context["observed_at"] = start["observed_at"].clone();
            context
        } else if self.supervisor.is_mock() {
            json!({"source":"mock_worker"})
        } else {
            return Ok(());
        };
        let workspace = Workspace::open(&session.internal_workspace_root)?;
        let mut record = workspace
            .get_operation(id)?
            .ok_or_else(|| ServiceError::OperationNotFound(id.to_string()))?;
        if record.details.execution_context.is_some() {
            return Ok(());
        }
        record.details.execution_context = Some(dbgatlas_model::ExecutionContext {
            worker: summary.worker,
            target,
            context,
        });
        // Sampling enriches the immutable execution context without deciding its result.
        record.status = operation.status;
        self.append_operation_result(&workspace, &record)
    }
}
