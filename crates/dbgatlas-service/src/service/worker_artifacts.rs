use super::*;

pub(super) struct RegisteredWorkerWrites {
    pub(super) artifacts: Vec<ArtifactRef>,
    pub(super) raw_output: Option<ArtifactRef>,
    pub(super) raw_output_byte_len: Option<u64>,
    pub(super) memory: Option<ArtifactRef>,
}

pub(super) fn finalize_debug_command_result(
    result: &mut DebugCommandResult,
    writes: &RegisteredWorkerWrites,
) {
    result.raw_output = writes.raw_output.clone();
    if result.full_output_artifact_ref.is_none() {
        result.full_output_artifact_ref = writes.raw_output.clone();
    }
    let full_output_byte_len = result
        .full_output_byte_len
        .or(writes.raw_output_byte_len)
        .unwrap_or(result.output.len() as u64);
    result.full_output_byte_len = Some(full_output_byte_len);
    result
        .inline_output_byte_limit
        .get_or_insert(DEFAULT_INLINE_TEXT_BYTE_LIMIT as u64);

    if result.output.len() > DEFAULT_INLINE_TEXT_BYTE_LIMIT {
        let preview = inline_text_preview(&result.output, DEFAULT_INLINE_TEXT_BYTE_LIMIT);
        result.output = preview.text;
        result.output_truncated = true;
    }
    if full_output_byte_len > DEFAULT_INLINE_TEXT_BYTE_LIMIT as u64 {
        result.output_truncated = true;
    }
    if result.output_truncated
        && !result
            .warnings
            .iter()
            .any(|warning| warning.contains("output truncated"))
    {
        result.warnings.push(format!(
            "output truncated to {} bytes inline; full output saved to raw output artifact",
            DEFAULT_INLINE_TEXT_BYTE_LIMIT
        ));
    }
}

pub(super) fn command_result_response(
    result: &DebugCommandResult,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    let mut value = serde_json::to_value(result)?;
    add_operation_refs(&mut value, status, writes, None)?;
    Ok(value)
}

pub(super) fn memory_result_response(
    result: &DebugMemoryResult,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    let mut value = serde_json::to_value(result)?;
    add_operation_refs(&mut value, status, writes, writes.memory.as_ref())?;
    Ok(value)
}

pub(super) fn add_operation_refs(
    value: &mut Value,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
    memory_ref: Option<&ArtifactRef>,
) -> Result<(), ServiceError> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    object.insert(
        "operation_status".to_string(),
        serde_json::to_value(&status)?,
    );
    object.insert(
        "artifact_refs".to_string(),
        serde_json::to_value(&writes.artifacts)?,
    );
    object.insert(
        "raw_output_ref".to_string(),
        serde_json::to_value(&writes.raw_output)?,
    );
    object.insert(
        "operation".to_string(),
        json!({
            "status": status,
            "artifact_refs": writes.artifacts,
            "raw_output_ref": writes.raw_output,
        }),
    );
    if let Some(memory_ref) = memory_ref {
        object.insert("memory_ref".to_string(), serde_json::to_value(memory_ref)?);
    }
    Ok(())
}

pub(super) fn register_worker_writes(
    workspace: &Workspace,
    operation_id: &OperationRef,
    writes: &[WorkerArtifactWrite],
) -> Result<RegisteredWorkerWrites, ServiceError> {
    let mut artifacts = Vec::new();
    let mut raw_output = None;
    let mut raw_output_byte_len = None;
    let mut memory = None;
    for write in writes {
        let artifact_id = next_artifact_ref();
        if write.kind == "debug.raw_output" {
            raw_output = Some(artifact_id.clone());
            raw_output_byte_len = Some(write.byte_len);
        }
        if write.kind == "debug.memory" {
            memory = Some(artifact_id.clone());
        }
        workspace.register_artifact(&ArtifactMetadata {
            artifact_id: artifact_id.clone(),
            kind: write.kind.clone(),
            relative_path: write.relative_path.clone(),
            created_at: Timestamp::now(),
            operation_id: Some(operation_id.clone()),
            byte_len: Some(write.byte_len),
            description: write.description.clone(),
        })?;
        artifacts.push(artifact_id);
    }
    Ok(RegisteredWorkerWrites {
        artifacts,
        raw_output,
        raw_output_byte_len,
        memory,
    })
}
