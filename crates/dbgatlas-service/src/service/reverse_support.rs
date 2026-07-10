use super::*;

pub(super) fn record_failed_reverse_operation(
    workspace: &Workspace,
    session_id: &SessionRef,
    artifact_dir: &Path,
    operation_id: &OperationRef,
    capability: &str,
    message: &str,
) -> Result<Vec<ArtifactRef>, ServiceError> {
    let value = json!({
        "operation_id": operation_id,
        "session_id": session_id,
        "capability": capability,
        "error": message,
        "created_at": Timestamp::now()
    });
    let error_file = format!("errors/{}.json", operation_id.id.as_str());
    let byte_len = write_json_file(&artifact_dir.join(&error_file), &value)?;
    let artifact_id = next_artifact_ref();
    workspace.register_artifact(&ArtifactMetadata {
        artifact_id: artifact_id.clone(),
        kind: "reverse.adapter_error".to_string(),
        relative_path: reverse_relative_path(session_id, &error_file),
        created_at: Timestamp::now(),
        operation_id: Some(operation_id.clone()),
        byte_len: Some(byte_len),
        description: Some("IDA adapter error".to_string()),
    })?;
    Ok(vec![artifact_id])
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_successful_reverse_core_operation(
    host: &ServiceHost,
    workspace: &Workspace,
    session: &ManagedSession,
    session_id: &SessionRef,
    operation_id: &OperationRef,
    capability: &str,
    arguments: Value,
    core: ReverseCoreFunctionResult,
) -> Result<Value, ServiceError> {
    let now = Timestamp::now();
    let event = json!({
        "operation_id": operation_id,
        "session_id": session_id,
        "created_at": now,
        "function": core.function,
        "arguments": arguments,
        "result": core.result,
        "warnings": core.warnings,
    });
    let core_file = format!("core/{}.jsonl", operation_id.id.as_str());
    let byte_len = write_jsonl_file(&session.artifact_dir.join(&core_file), &event)?;
    let artifact_id = next_artifact_ref();
    let response_result = reverse_core_response_result(&core, &artifact_id, byte_len)?;
    let normalized_addresses = collect_normalized_addresses(&response_result);
    workspace.register_artifact(&ArtifactMetadata {
        artifact_id: artifact_id.clone(),
        kind: "reverse.core".to_string(),
        relative_path: reverse_relative_path(session_id, &core_file),
        created_at: now,
        operation_id: Some(operation_id.clone()),
        byte_len: Some(byte_len),
        description: Some(format!("IDA Core Function {} result", core.function)),
    })?;
    workspace.append_operation(&OperationRecord {
        operation_id: operation_id.clone(),
        adapter_id: "ida".to_string(),
        capability: capability.to_string(),
        status: OperationStatus::Success,
        created_at: now,
        summary: format!("IDA Core Function {} completed", core.function),
        artifacts: vec![artifact_id.clone()],
        raw_output: None,
    })?;

    let mut operation = ServiceOperation::success(
        operation_id.clone(),
        capability,
        Some(session_id.clone()),
        "reverse core function completed",
    );
    operation.artifacts = vec![artifact_id.clone()];
    let mut state = host.lock_state()?;
    state
        .operations
        .insert(operation_id.id.as_str().to_string(), operation);
    if let Some(session) = state.sessions.get_mut(session_id.id.as_str()) {
        session.updated_at = now;
        session.last_operation = Some(operation_id.clone());
        session.artifacts.push(artifact_id.clone());
    }

    let mut response = json!({
        "session_id": session_id,
        "operation_id": operation_id,
        "operation_status": "success",
        "artifact_refs": [artifact_id],
        "function": core.function,
        "result": response_result,
        "warnings": core.warnings,
        "operation": {
            "status": "success",
            "artifact_refs": [artifact_id],
            "raw_output_ref": null
        }
    });
    if !normalized_addresses.is_empty() {
        response["normalized_addresses"] = Value::Array(normalized_addresses);
        response["address_warnings"] = Value::Array(Vec::new());
    }
    Ok(response)
}

pub(super) fn reverse_core_response_result(
    core: &ReverseCoreFunctionResult,
    artifact_id: &ArtifactRef,
    artifact_byte_len: u64,
) -> Result<Value, ServiceError> {
    if core.function != "decompile"
        || serde_json::to_vec(&core.result)?.len() <= MAX_INLINE_REVERSE_RESULT_BYTES
    {
        return Ok(core.result.clone());
    }

    let mut result = core.result.clone();
    if let Value::Object(object) = &mut result {
        if let Some(Value::String(pseudocode)) = object.get_mut("pseudocode") {
            if pseudocode.len() > MAX_INLINE_DECOMPILE_PSEUDOCODE_BYTES {
                *pseudocode =
                    truncate_utf8_bytes(pseudocode, MAX_INLINE_DECOMPILE_PSEUDOCODE_BYTES)
                        .to_string();
            }
        }
        object.insert("pseudocode_truncated".to_string(), json!(true));
        object.insert(
            "full_result_artifact_ref".to_string(),
            serde_json::to_value(artifact_id)?,
        );
        object.insert("full_result_byte_len".to_string(), json!(artifact_byte_len));
        return Ok(result);
    }

    Ok(json!({
        "pseudocode_truncated": true,
        "preview": result,
        "full_result_artifact_ref": artifact_id,
        "full_result_byte_len": artifact_byte_len,
    }))
}

pub(super) fn collect_normalized_addresses(value: &Value) -> Vec<Value> {
    pub(super) fn visit(value: &Value, out: &mut Vec<Value>) {
        match value {
            Value::Object(object) => {
                if let Some(address) = object.get("normalized_address") {
                    out.push(address.clone());
                }
                for child in object.values() {
                    visit(child, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, out);
                }
            }
            _ => {}
        }
    }

    let mut addresses = Vec::new();
    visit(value, &mut addresses);
    addresses
}

pub(super) fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(super) fn recording_response(
    recording: &ManagedRecording,
    operation_id: &OperationRef,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    Ok(json!({
        "recording_id": recording.recording_id,
        "state": recording.state,
        "target": recording.target,
        "root_pid": recording.root_pid,
        "presets": recording.presets,
        "operation_id": operation_id,
        "operation_status": status,
        "artifact_refs": writes.artifacts,
        "raw_output_ref": writes.raw_output,
        "operation": {
            "status": status,
            "artifact_refs": writes.artifacts,
            "raw_output_ref": writes.raw_output,
        }
    }))
}

pub(super) fn recording_status_response(recording: &ManagedRecording) -> Value {
    json!({
        "recording_id": recording.recording_id,
        "state": recording.state,
        "target": recording.target,
        "root_pid": recording.root_pid,
        "presets": recording.presets,
        "created_at": recording.created_at,
        "updated_at": recording.updated_at,
        "last_operation": recording.last_operation,
        "artifact_refs": recording.artifacts,
    })
}
