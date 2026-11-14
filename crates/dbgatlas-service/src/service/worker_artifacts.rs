use super::*;

#[derive(Default)]
pub(super) struct RegisteredWorkerWrites {
    pub(super) artifacts: Vec<ArtifactRef>,
    pub(super) raw_output: Option<ArtifactRef>,
    pub(super) raw_output_byte_len: Option<u64>,
    pub(super) full_result: Option<ArtifactRef>,
    pub(super) memory: Option<ArtifactRef>,
}

pub(super) fn finalize_debug_command_result(
    result: &mut DebugCommandResult,
    writes: &RegisteredWorkerWrites,
) {
    if let Some(full_byte_len) = writes.raw_output_byte_len {
        result.result.full_byte_len = full_byte_len;
        result.result.truncated = full_byte_len > result.result.value.len() as u64;
    }
}

pub(super) fn command_result_response(
    result: &DebugCommandResult,
    status: OperationStatus,
    writes: &RegisteredWorkerWrites,
) -> Result<Value, ServiceError> {
    let full_artifact_ref = writes
        .full_result
        .clone()
        .or_else(|| writes.raw_output.clone())
        .ok_or_else(|| {
            ServiceError::Worker("debug command did not produce a full result artifact".to_string())
        })?;
    let mut value = json!({
        "session_id": result.session_id,
        "operation_id": result.operation_id,
        "command": result.command,
        "result": result.result.value,
        "result_delivery": ResultDelivery {
            truncated: result.result.truncated,
            full_byte_len: result.result.full_byte_len,
            inline_byte_limit: result.result.inline_byte_limit,
            full_artifact_ref,
        },
        "final_state": result.final_state,
        "warnings": result.warnings,
        "error": result.error,
    });
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
    let mut full_result = None;
    let mut memory = None;
    for write in writes {
        if write.role == Some(WorkerArtifactRole::FullResult) {
            let path = workspace.resolve_artifact_relative_path(&write.relative_path)?;
            let actual_len = fs::metadata(&path)?.len();
            if actual_len != write.byte_len {
                return Err(ServiceError::Worker(format!(
                    "full result artifact length mismatch for {}: reported {}, actual {actual_len}",
                    write.relative_path.display(),
                    write.byte_len
                )));
            }
        }
        let artifact_id = next_artifact_ref();
        if write.kind == "debug.raw_output" {
            raw_output = Some(artifact_id.clone());
            raw_output_byte_len = Some(write.byte_len);
        }
        if write.role == Some(WorkerArtifactRole::FullResult) {
            full_result = Some(artifact_id.clone());
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
        full_result,
        memory,
    })
}

pub(super) fn recover_full_result_write(
    workspace: &Workspace,
    operation_id: &OperationRef,
    absolute_path: &Path,
    relative_path: PathBuf,
    kind: &str,
    description: &str,
) -> Result<RegisteredWorkerWrites, ServiceError> {
    if !absolute_path.is_file() {
        return Ok(RegisteredWorkerWrites {
            artifacts: Vec::new(),
            raw_output: None,
            raw_output_byte_len: None,
            full_result: None,
            memory: None,
        });
    }
    register_worker_writes(
        workspace,
        operation_id,
        &[WorkerArtifactWrite {
            relative_path,
            kind: kind.to_string(),
            role: Some(WorkerArtifactRole::FullResult),
            byte_len: fs::metadata(absolute_path)?.len(),
            description: Some(description.to_string()),
        }],
    )
}

pub(super) fn cleanup_partial_result_write(absolute_final_path: &Path) -> std::io::Result<()> {
    let mut part_name = absolute_final_path.as_os_str().to_os_string();
    part_name.push(".part");
    let part_path = PathBuf::from(part_name);
    match fs::remove_file(part_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_partial_result_removes_only_the_part_file() {
        let temp = tempfile::tempdir().unwrap();
        let final_path = temp.path().join("result.json");
        let part_path = temp.path().join("result.json.part");
        fs::write(&final_path, b"complete").unwrap();
        fs::write(&part_path, b"partial").unwrap();

        cleanup_partial_result_write(&final_path).unwrap();

        assert_eq!(fs::read(&final_path).unwrap(), b"complete");
        assert!(!part_path.exists());
    }
}
