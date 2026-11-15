use super::*;

pub(super) fn ensure_project_workspace(project_root: &Path) -> Result<Workspace, ServiceError> {
    let workspace_root = project_root.join(INTERNAL_WORKSPACE_DIR);
    match Workspace::open(&workspace_root) {
        Ok(workspace) => Ok(workspace),
        Err(WorkspaceError::ManifestNotFound(_)) => {
            Ok(Workspace::init(workspace_root, Default::default())?)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn workspace_facts_with_fallback(
    project_root: &Path,
) -> Result<WorkspaceFacts, ServiceError> {
    let internal_workspace = project_root.join(INTERNAL_WORKSPACE_DIR);
    match Workspace::open(&internal_workspace) {
        Ok(workspace) => return Ok(workspace.facts()?),
        Err(WorkspaceError::ManifestNotFound(_)) => {}
        Err(error) => return Err(error.into()),
    }

    let artifacts_root = internal_workspace.join("artifacts");
    if artifacts_root.is_dir() {
        return fallback_workspace_facts_from_artifacts(&artifacts_root);
    }

    Err(WorkspaceError::ManifestNotFound(internal_workspace.join("dbgatlas-workspace.json")).into())
}

pub(super) fn fallback_workspace_facts_from_artifacts(
    artifacts_root: &Path,
) -> Result<WorkspaceFacts, ServiceError> {
    let mut artifacts: Vec<ArtifactMetadata> =
        read_json_lines_optional(&artifacts_root.join("artifacts.jsonl"))?;
    if artifacts.is_empty() {
        artifacts = scan_synthetic_artifacts(artifacts_root)?;
    }
    Ok(WorkspaceFacts {
        artifacts,
        operations: read_json_lines_optional(&artifacts_root.join("operations.jsonl"))?,
        command_audit: read_json_lines_optional(&artifacts_root.join("command_audit.jsonl"))?,
    })
}

pub(super) fn read_json_lines_optional<T: DeserializeOwned>(
    path: &Path,
) -> Result<Vec<T>, ServiceError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)?;
    let mut values = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        values.push(serde_json::from_str(line)?);
    }
    Ok(values)
}

pub(super) fn scan_synthetic_artifacts(
    artifacts_root: &Path,
) -> Result<Vec<ArtifactMetadata>, ServiceError> {
    let workspace_root = artifacts_root.parent().unwrap_or(artifacts_root);
    let mut files = Vec::new();
    collect_artifact_files(artifacts_root, &mut files)?;
    files.sort();

    let mut artifacts = Vec::new();
    for (index, path) in files.into_iter().enumerate() {
        let relative_path = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .to_path_buf();
        let byte_len = fs::metadata(&path).ok().map(|metadata| metadata.len());
        artifacts.push(ArtifactMetadata {
            producer_session_id: None,
            producer_execution_id: None,
            artifact_id: ArtifactRef::new(
                Id::new(format!("artifact-synthetic-{}", index + 1))
                    .expect("synthetic artifact ids are valid"),
            ),
            kind: infer_synthetic_artifact_kind(artifacts_root, &path),
            relative_path,
            created_at: Timestamp::now(),
            operation_id: None,
            byte_len,
            description: Some("discovered artifact (workspace manifest missing)".to_string()),
        });
    }
    Ok(artifacts)
}

pub(super) fn collect_artifact_files(
    dir: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), ServiceError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_artifact_files(&path, files)?;
        } else if file_type.is_file() && !is_workspace_index_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

pub(super) fn is_workspace_index_file(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            matches!(
                name,
                "artifacts.jsonl" | "operations.jsonl" | "command_audit.jsonl"
            )
        })
}

pub(super) fn infer_synthetic_artifact_kind(artifacts_root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(artifacts_root).unwrap_or(path);
    let components = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    match components.as_slice() {
        ["recordings", ..] if file_name == "recording.json" => "recording.metadata".to_string(),
        ["recordings", ..] if file_name == "events.jsonl" => "recording.events.ttd".to_string(),
        ["recordings", _, "events", event_file] => {
            let category = event_file.strip_suffix(".jsonl").unwrap_or(event_file);
            format!("recording.events.{category}")
        }
        ["recordings", ..] if file_name == "trace.etl" || file_name == "trace.unfiltered.etl" => {
            "recording.trace".to_string()
        }
        ["recordings", _, "cpu", "hotspots.json"] => "recording.cpu.hotspots".to_string(),
        ["recordings", _, "cpu", "stacks.folded"] => "recording.cpu.folded".to_string(),
        ["recordings", ..] if extension == "run" => "recording.ttd.trace".to_string(),
        ["recordings", ..] if extension == "idx" => "recording.ttd.index".to_string(),
        ["recordings", ..] if file_name.starts_with("recorder") => {
            "recording.recorder_output".to_string()
        }
        ["sessions", ..] => "debug.artifact".to_string(),
        _ => "artifact.file".to_string(),
    }
}
