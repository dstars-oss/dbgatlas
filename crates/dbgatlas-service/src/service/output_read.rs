use super::*;
use std::hash::{BuildHasher, Hash, Hasher};
use std::io::{Seek, SeekFrom};

#[derive(Default)]
pub(super) struct Cursors {
    signer: std::collections::hash_map::RandomState,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Position {
    pub resource: String,
    pub offsets: Vec<u64>,
    pub artifacts: usize,
    pub after: Option<String>,
    #[serde(default)]
    pub gap: bool,
}

impl Cursors {
    pub fn encode(&self, position: &Position) -> String {
        let bytes = serde_json::to_vec(position).unwrap();
        let mut signature = self.signer.build_hasher();
        bytes.hash(&mut signature);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        format!("{hex}.{:016x}", signature.finish())
    }
    pub fn decode(&self, token: &str, resource: &str) -> Result<Position, ServiceError> {
        let invalid = || {
            ServiceError::Rpc("invalid cursor, wrong resource, or previous service instance".into())
        };
        if token.len() > 8192 {
            return Err(invalid());
        }
        let (hex, signature) = token.split_once('.').ok_or_else(invalid)?;
        if hex.len() % 2 != 0 {
            return Err(invalid());
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| {
                hex.get(i..i + 2)
                    .and_then(|s| u8::from_str_radix(s, 16).ok())
                    .ok_or_else(invalid)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut expected = self.signer.build_hasher();
        bytes.hash(&mut expected);
        if signature != format!("{:016x}", expected.finish()) {
            return Err(invalid());
        }
        let position: Position = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if position.resource != resource {
            return Err(invalid());
        }
        Ok(position)
    }
}

impl ServiceHost {
    pub(super) fn output_read(
        &self,
        params: Option<Value>,
        session_stream: bool,
    ) -> Result<Value, ServiceError> {
        let params = params.unwrap_or_default();
        let max_bytes = match params.get("max_bytes") {
            None => 8192,
            Some(v) => v
                .as_u64()
                .filter(|n| (4..=65536).contains(n))
                .ok_or_else(|| ServiceError::Rpc("max_bytes must be from 4 through 65536".into()))?
                as usize,
        };
        let (resource, workspace, paths, artifacts, finished, mut gap) = if session_stream {
            let sid: SessionRef = serde_json::from_value(params["session_id"].clone())?;
            let observed = self.observed_session(&sid)?;
            let session = self
                .lock_state()?
                .summaries
                .get(sid.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(sid.to_string()))?;
            if session.kind != dbgatlas_model::SessionKind::Debug {
                return Err(ServiceError::Rpc("session.read currently supports debug sessions; use recording artifact references".into()));
            }
            let workspace = Workspace::open(&session.workspace.root)?;
            let dir = workspace.ensure_session_artifact_dir(&sid.id)?;
            let ended = session.lifecycle.is_terminal() && session.cleanup.complete;
            let error = session.lifecycle == dbgatlas_model::SessionLifecycle::Failed
                || observed["context"]["output"]["error"].as_u64().unwrap_or(0) != 0
                || observed["context"]["engine"]["event_gap"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0
                || observed["context"]["output"]
                    .get("event_log_error")
                    .is_some_and(|e| !e.is_null());
            (
                format!("session:{sid}"),
                workspace,
                vec![
                    ("native_output", dir.join("session-output.log")),
                    ("engine_events", dir.join("engine-events.jsonl")),
                    ("lifecycle_events", dir.join("session-events.jsonl")),
                ],
                session.artifacts,
                ended,
                error,
            )
        } else {
            let id: OperationRef = serde_json::from_value(params["execution_id"].clone())?;
            let op = self
                .lock_state()?
                .operations
                .get(id.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::OperationNotFound(id.to_string()))?;
            let sid = op
                .session_id
                .clone()
                .or(op.details.session_id.clone())
                .ok_or_else(|| ServiceError::Rpc("execution has no session output scope".into()))?;
            let session = self
                .lock_state()?
                .summaries
                .get(sid.id.as_str())
                .cloned()
                .ok_or_else(|| ServiceError::SessionNotFound(sid.to_string()))?;
            let workspace = Workspace::open(&session.workspace.root)?;
            let dir = workspace.ensure_session_artifact_dir(&sid.id)?;
            let finished = self
                .execution_job(&id)
                .map(|j| j.state.lock().unwrap().finished)
                .unwrap_or_else(|_| execution::terminal(&op.status));
            let mut paths = Vec::new();
            if let Some(raw) = &op.raw_output {
                if let Some(meta) = workspace.get_artifact(raw)? {
                    paths.push((
                        "native_output",
                        workspace.resolve_artifact_relative_path(&meta.relative_path)?,
                    ));
                }
            }
            // The execution ID is registered before dispatch; the worker contract
            // assigns this path, so callers never supply a filesystem location.
            if paths.is_empty()
                && matches!(
                    execution::action_name(&op.capability),
                    "session.exec" | "debug.add_symbols" | "debug.eval_steps.step"
                )
            {
                paths.push((
                    "native_output",
                    dir.join("raw").join(format!("{}.txt", id.id)),
                ));
            }
            let gap = op.details.output_error.is_some() || op.status == OperationStatus::Unknown;
            (
                format!("execution:{id}"),
                workspace,
                paths,
                op.artifacts,
                finished,
                gap,
            )
        };
        let mut position = if let Some(token) = params.get("cursor").filter(|v| !v.is_null()) {
            self.executions.cursors.lock().unwrap().decode(
                token
                    .as_str()
                    .ok_or_else(|| ServiceError::Rpc("cursor must be a string".into()))?,
                &resource,
            )?
        } else {
            Position {
                resource: resource.clone(),
                offsets: vec![0; paths.len()],
                ..Default::default()
            }
        };
        if position.offsets.len() != paths.len() {
            return Err(ServiceError::Rpc("output source layout changed".into()));
        }
        gap |= position.gap;
        let mut remaining = max_bytes;
        let mut items = Vec::new();
        let mut has_more = false;
        for (index, (source, final_path)) in paths.iter().enumerate() {
            let path = if final_path.is_file() {
                final_path.clone()
            } else {
                let mut part = final_path.as_os_str().to_os_string();
                part.push(".part");
                PathBuf::from(part)
            };
            let relative = path
                .strip_prefix(workspace.root())
                .map_err(|_| ServiceError::Rpc("output is outside its workspace".into()))?;
            let mut file = match workspace.open_artifact_read(relative) {
                Ok(file) => file,
                Err(error) if !path.exists() => {
                    if finished && !session_stream && !paths.is_empty() {
                        gap = true;
                    }
                    let _ = error;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let committed = file.metadata()?.len();
            let offset = position.offsets[index];
            if offset > committed {
                gap = true;
                continue;
            }
            if offset == committed {
                continue;
            }
            if remaining < 4 {
                has_more = true;
                continue;
            }
            file.seek(SeekFrom::Start(offset))?;
            let allocation = (remaining / (paths.len() - index)).max(4).min(remaining);
            let mut bytes =
                vec![0; allocation.min((committed - offset).min(usize::MAX as u64) as usize)];
            let count = file.read(&mut bytes)?;
            bytes.truncate(count);
            // Hold incomplete UTF-8 tails for a later read, including tails split
            // by the budget. Invalid native byte sequences remain explicit loss.
            let mut consumed = bytes.len();
            let mut checked = 0;
            while checked < bytes.len() {
                match std::str::from_utf8(&bytes[checked..]) {
                    Ok(_) => break,
                    Err(error) => {
                        checked += error.valid_up_to();
                        if let Some(length) = error.error_len() {
                            gap = true;
                            checked += length;
                        } else {
                            consumed = checked;
                            break;
                        }
                    }
                }
            }
            if consumed == 0 && finished && offset + count as u64 == committed {
                gap = true;
                consumed = count;
            }
            if consumed > 0 {
                let text = String::from_utf8_lossy(&bytes[..consumed]).into_owned();
                items.push(json!({"kind":"text","source":source,"text":text}));
                position.offsets[index] += consumed as u64;
                remaining -= consumed;
            }
            has_more |= position.offsets[index] < committed;
        }
        for artifact in artifacts.iter().skip(position.artifacts).take(64) {
            items.push(json!({"kind":"artifact","artifact_ref":artifact}));
            position.artifacts += 1;
        }
        has_more |= position.artifacts < artifacts.len();
        position.gap = gap;
        let cursor = self.executions.cursors.lock().unwrap().encode(&position);
        Ok(
            json!({"items":items,"next_cursor":cursor,"has_more":has_more,"complete":finished && !has_more && !gap,"gap":gap}),
        )
    }

    pub(super) fn execution_list(&self, params: Option<Value>) -> Result<Value, ServiceError> {
        let params = params.unwrap_or_default();
        let project = params["project_root"]
            .as_str()
            .ok_or_else(|| ServiceError::Rpc("project_root is required".into()))?;
        let facts = workspace_facts_with_fallback(Path::new(project))?;
        let session: Option<SessionRef> = params
            .get("session_id")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?;
        let status: Option<OperationStatus> = params
            .get("status")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?;
        let limit = params
            .get("limit")
            .map(|v| {
                v.as_u64()
                    .filter(|v| (1..=100).contains(v))
                    .ok_or_else(|| ServiceError::Rpc("limit must be from 1 through 100".into()))
            })
            .transpose()?
            .unwrap_or(20) as usize;
        let resource = format!(
            "execution.list:{}:{}:{}",
            fs::canonicalize(project)?.display(),
            serde_json::to_string(&session)?,
            serde_json::to_string(&status)?
        );
        let mut position = if let Some(token) = params["cursor"].as_str() {
            self.executions
                .cursors
                .lock()
                .unwrap()
                .decode(token, &resource)?
        } else {
            Position {
                resource,
                ..Default::default()
            }
        };
        let mut values = Vec::new();
        for record in facts.operations {
            if session
                .as_ref()
                .is_some_and(|id| record.details.session_id.as_ref() != Some(id))
            {
                continue;
            }
            let value=self.execution_value(&record.operation_id).unwrap_or_else(|_| {
                let observed_status=if execution::terminal(&record.status) {record.status} else {OperationStatus::Unknown};
                json!({"execution_id":record.operation_id,"session_id":record.details.session_id,"action":execution::action_name(&record.capability),"input":record.details.input,"status":observed_status,"sequence":record.details.update_seq,"execution_context":record.details.execution_context,"artifact_refs":record.artifacts,"raw_output_ref":record.raw_output,"output_error":record.details.output_error,"created_at":record.created_at,"historical":true})
            });
            if status.as_ref().is_some_and(|s| value["status"] != json!(s)) {
                continue;
            }
            let key = format!(
                "{:020}:{}",
                record.created_at.unix_millis, record.operation_id.id
            );
            if position.after.as_ref().is_some_and(|after| &key <= after) {
                continue;
            }
            values.push((key, value));
        }
        values.sort_by(|a, b| a.0.cmp(&b.0));
        let has_more = values.len() > limit;
        values.truncate(limit);
        if let Some((key, _)) = values.last() {
            position.after = Some(key.clone());
        }
        let cursor = self.executions.cursors.lock().unwrap().encode(&position);
        Ok(
            json!({"executions":values.into_iter().map(|(_,value)|value).collect::<Vec<_>>(),"next_cursor":cursor,"has_more":has_more}),
        )
    }
}
