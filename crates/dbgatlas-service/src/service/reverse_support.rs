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
    writes: RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    let now = Timestamp::now();
    let full_artifact_ref = writes.full_result.clone().ok_or_else(|| {
        ServiceError::Worker(
            "reverse core function did not produce a full result artifact".to_string(),
        )
    })?;
    let delivery = ResultDelivery {
        truncated: core.result.truncated,
        full_byte_len: core.result.full_byte_len,
        inline_byte_limit: core.result.inline_byte_limit,
        full_artifact_ref: full_artifact_ref.clone(),
    };
    let event = json!({
        "operation_id": operation_id,
        "session_id": session_id,
        "created_at": now,
        "function": core.function,
        "arguments": arguments,
        "result_artifact_ref": full_artifact_ref,
        "result_delivery": delivery,
        "warnings": core.warnings,
    });
    let core_file = format!("core/{}.jsonl", operation_id.id.as_str());
    let byte_len = write_jsonl_file(&session.artifact_dir.join(&core_file), &event)?;
    let audit_artifact_id = next_artifact_ref();
    let normalized_addresses = collect_normalized_addresses(&core.result.value);
    workspace.register_artifact(&ArtifactMetadata {
        artifact_id: audit_artifact_id.clone(),
        kind: "reverse.core".to_string(),
        relative_path: reverse_relative_path(session_id, &core_file),
        created_at: now,
        operation_id: Some(operation_id.clone()),
        byte_len: Some(byte_len),
        description: Some(format!("IDA Core Function {} audit event", core.function)),
    })?;
    let mut artifacts = writes.artifacts.clone();
    artifacts.push(audit_artifact_id.clone());
    workspace.append_operation(&OperationRecord {
        operation_id: operation_id.clone(),
        adapter_id: "ida".to_string(),
        capability: capability.to_string(),
        status: OperationStatus::Success,
        created_at: now,
        summary: format!("IDA Core Function {} completed", core.function),
        artifacts: artifacts.clone(),
        raw_output: None,
    })?;

    let mut operation = ServiceOperation::success(
        operation_id.clone(),
        capability,
        Some(session_id.clone()),
        "reverse core function completed",
    );
    operation.artifacts = artifacts.clone();
    let mut state = host.lock_state()?;
    state
        .operations
        .insert(operation_id.id.as_str().to_string(), operation);
    if let Some(session) = state.sessions.get_mut(session_id.id.as_str()) {
        session.updated_at = now;
        session.last_operation = Some(operation_id.clone());
        session.artifacts.extend(artifacts.clone());
    }

    let mut response = json!({
        "session_id": session_id,
        "operation_id": operation_id,
        "operation_status": "success",
        "artifact_refs": artifacts,
        "function": core.function,
        "result": core.result.value,
        "result_delivery": delivery,
        "warnings": core.warnings,
        "operation": {
            "status": "success",
            "artifact_refs": artifacts,
            "raw_output_ref": null
        }
    });
    if !normalized_addresses.is_empty() {
        response["normalized_addresses"] = Value::Array(normalized_addresses);
        response["address_warnings"] = Value::Array(Vec::new());
    }
    Ok(response)
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
