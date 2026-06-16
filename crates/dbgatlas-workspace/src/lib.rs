use dbgatlas_model::{ArtifactRef, Id, InvalidId, OperationRef, Timestamp};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub const MANIFEST_FILE: &str = "dbgatlas-workspace.json";
pub const ARTIFACTS_DIR: &str = "artifacts";
pub const ANALYSIS_DIR: &str = "analysis";
pub const INPUTS_DIR: &str = "inputs";
pub const ARTIFACTS_LOG: &str = "artifacts/artifacts.jsonl";
pub const OPERATIONS_LOG: &str = "artifacts/operations.jsonl";
pub const COMMAND_AUDIT_LOG: &str = "artifacts/command_audit.jsonl";
pub const SESSIONS_DIR: &str = "sessions";
pub const PROFILES_DIR: &str = "profiles";
pub const TTD_RECORDINGS_DIR: &str = "ttd_recordings";
pub const REVERSE_SESSIONS_DIR: &str = "reverse_sessions";

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace already exists at {0}")]
    AlreadyExists(PathBuf),
    #[error("workspace manifest not found at {0}")]
    ManifestNotFound(PathBuf),
    #[error("invalid path segment `{0}`")]
    InvalidPathSegment(String),
    #[error("artifact path must be relative and stay under artifacts/: {0}")]
    InvalidArtifactPath(PathBuf),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json error at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    InvalidId(#[from] InvalidId),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceManifest {
    pub schema_version: u32,
    pub workspace_id: Id,
    pub created_at: Timestamp,
    pub tool: ToolInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceInitOptions {
    pub create_inputs: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub root: PathBuf,
    pub manifest: WorkspaceManifest,
    pub has_artifacts_dir: bool,
    pub has_analysis_dir: bool,
    pub has_inputs_dir: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactMetadata {
    pub artifact_id: ArtifactRef,
    pub kind: String,
    pub relative_path: PathBuf,
    pub created_at: Timestamp,
    pub operation_id: Option<OperationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<u64>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRecord {
    pub operation_id: OperationRef,
    pub adapter_id: String,
    pub capability: String,
    pub status: OperationStatus,
    pub created_at: Timestamp,
    pub summary: String,
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<ArtifactRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandAuditRecord {
    pub operation_id: OperationRef,
    pub session_id: Option<dbgatlas_model::SessionRef>,
    pub capability: String,
    pub command: String,
    pub created_at: Timestamp,
    pub status: OperationStatus,
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<ArtifactRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceFacts {
    pub artifacts: Vec<ArtifactMetadata>,
    pub operations: Vec<OperationRecord>,
    pub command_audit: Vec<CommandAuditRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    #[serde(alias = "Running")]
    Running,
    #[serde(alias = "Success")]
    Success,
    #[serde(alias = "Failed")]
    Failed,
    #[serde(alias = "Canceled")]
    Canceled,
}

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    manifest: WorkspaceManifest,
}

impl Workspace {
    pub fn init(
        root: impl Into<PathBuf>,
        options: WorkspaceInitOptions,
    ) -> Result<Self, WorkspaceError> {
        let root = root.into();
        let manifest_path = root.join(MANIFEST_FILE);
        if manifest_path.exists() {
            return Err(WorkspaceError::AlreadyExists(root));
        }

        create_dir(&root)?;
        create_dir(root.join(ARTIFACTS_DIR))?;
        create_dir(root.join(ANALYSIS_DIR))?;
        if options.create_inputs {
            create_dir(root.join(INPUTS_DIR))?;
        }

        let workspace_name = root
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("workspace");

        let manifest = WorkspaceManifest {
            schema_version: 1,
            workspace_id: Id::new(workspace_name)?,
            created_at: Timestamp::now(),
            tool: ToolInfo {
                name: "DbgAtlas".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        write_json_pretty(&manifest_path, &manifest)?;
        Ok(Self { root, manifest })
    }

    pub fn open(root: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let root = root.into();
        let manifest_path = root.join(MANIFEST_FILE);
        if !manifest_path.exists() {
            return Err(WorkspaceError::ManifestNotFound(manifest_path));
        }
        let manifest = read_json(&manifest_path)?;
        Ok(Self { root, manifest })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &WorkspaceManifest {
        &self.manifest
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    pub fn info(&self) -> WorkspaceInfo {
        WorkspaceInfo {
            root: self.root.clone(),
            manifest: self.manifest.clone(),
            has_artifacts_dir: self.root.join(ARTIFACTS_DIR).is_dir(),
            has_analysis_dir: self.root.join(ANALYSIS_DIR).is_dir(),
            has_inputs_dir: self.root.join(INPUTS_DIR).is_dir(),
        }
    }

    pub fn allocate_artifact_path(
        &self,
        kind: &str,
        file_name: &str,
    ) -> Result<PathBuf, WorkspaceError> {
        validate_path_segment(kind)?;
        validate_path_segment(file_name)?;
        let dir = self.root.join(ARTIFACTS_DIR).join(kind);
        create_dir(&dir)?;
        Ok(dir.join(file_name))
    }

    pub fn ensure_session_artifact_dir(&self, session_id: &Id) -> Result<PathBuf, WorkspaceError> {
        self.ensure_domain_artifact_dir(SESSIONS_DIR, session_id)
    }

    pub fn ensure_profile_artifact_dir(&self, profile_id: &Id) -> Result<PathBuf, WorkspaceError> {
        self.ensure_domain_artifact_dir(PROFILES_DIR, profile_id)
    }

    pub fn ensure_ttd_recording_artifact_dir(
        &self,
        recording_id: &Id,
    ) -> Result<PathBuf, WorkspaceError> {
        self.ensure_domain_artifact_dir(TTD_RECORDINGS_DIR, recording_id)
    }

    pub fn ensure_reverse_session_artifact_dir(
        &self,
        session_id: &Id,
    ) -> Result<PathBuf, WorkspaceError> {
        self.ensure_domain_artifact_dir(REVERSE_SESSIONS_DIR, session_id)
    }

    pub fn resolve_artifact_relative_path(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<PathBuf, WorkspaceError> {
        let relative_path = relative_path.as_ref();
        validate_artifact_relative_path(relative_path)?;
        let path = self.root.join(relative_path);
        self.ensure_resolved_artifact_path_is_contained(&path)?;
        Ok(path)
    }

    pub fn register_artifact(&self, metadata: &ArtifactMetadata) -> Result<(), WorkspaceError> {
        self.resolve_artifact_relative_path(&metadata.relative_path)?;
        append_json_line(self.root.join(ARTIFACTS_LOG), metadata)
    }

    pub fn list_artifacts(&self) -> Result<Vec<ArtifactMetadata>, WorkspaceError> {
        read_json_lines(self.root.join(ARTIFACTS_LOG))
    }

    pub fn get_artifact(
        &self,
        artifact_id: &ArtifactRef,
    ) -> Result<Option<ArtifactMetadata>, WorkspaceError> {
        Ok(self
            .list_artifacts()?
            .into_iter()
            .find(|artifact| artifact.artifact_id == *artifact_id))
    }

    pub fn append_operation(&self, record: &OperationRecord) -> Result<(), WorkspaceError> {
        append_json_line(self.root.join(OPERATIONS_LOG), record)
    }

    pub fn list_operations(&self) -> Result<Vec<OperationRecord>, WorkspaceError> {
        read_json_lines(self.root.join(OPERATIONS_LOG))
    }

    pub fn get_operation(
        &self,
        operation_id: &OperationRef,
    ) -> Result<Option<OperationRecord>, WorkspaceError> {
        Ok(self
            .list_operations()?
            .into_iter()
            .rev()
            .find(|operation| operation.operation_id == *operation_id))
    }

    pub fn append_command_audit(&self, record: &CommandAuditRecord) -> Result<(), WorkspaceError> {
        append_json_line(self.root.join(COMMAND_AUDIT_LOG), record)
    }

    pub fn list_command_audit(&self) -> Result<Vec<CommandAuditRecord>, WorkspaceError> {
        read_json_lines(self.root.join(COMMAND_AUDIT_LOG))
    }

    pub fn facts(&self) -> Result<WorkspaceFacts, WorkspaceError> {
        Ok(WorkspaceFacts {
            artifacts: self.list_artifacts()?,
            operations: self.list_operations()?,
            command_audit: self.list_command_audit()?,
        })
    }

    pub fn append_event_jsonl<T: Serialize>(
        &self,
        relative_path: impl AsRef<Path>,
        event: &T,
    ) -> Result<(), WorkspaceError> {
        let path = self.resolve_artifact_relative_path(relative_path)?;
        append_json_line(path, event)
    }

    pub fn append_text_artifact(
        &self,
        relative_path: impl AsRef<Path>,
        text: &str,
    ) -> Result<(), WorkspaceError> {
        let path = self.resolve_artifact_relative_path(relative_path)?;
        if let Some(parent) = path.parent() {
            create_dir(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| WorkspaceError::Io {
                path: path.clone(),
                source,
            })?;
        file.write_all(text.as_bytes())
            .map_err(|source| WorkspaceError::Io { path, source })
    }

    fn ensure_domain_artifact_dir(&self, domain: &str, id: &Id) -> Result<PathBuf, WorkspaceError> {
        let dir = self.root.join(ARTIFACTS_DIR).join(domain).join(id.as_str());
        create_dir(&dir)?;
        Ok(dir)
    }

    fn ensure_resolved_artifact_path_is_contained(
        &self,
        path: &Path,
    ) -> Result<(), WorkspaceError> {
        let artifact_root = canonicalize_existing_path(&self.root.join(ARTIFACTS_DIR))?;
        let workspace_root = canonicalize_existing_path(&self.root)?;
        if !artifact_root.starts_with(&workspace_root) {
            return Err(WorkspaceError::InvalidArtifactPath(PathBuf::from(
                ARTIFACTS_DIR,
            )));
        }

        let existing_ancestor = nearest_existing_ancestor(path);
        let canonical_ancestor = canonicalize_existing_path(&existing_ancestor)?;
        if !canonical_ancestor.starts_with(&artifact_root) {
            return Err(WorkspaceError::InvalidArtifactPath(
                path.strip_prefix(&self.root).unwrap_or(path).to_path_buf(),
            ));
        }

        Ok(())
    }
}

fn validate_path_segment(value: &str) -> Result<(), WorkspaceError> {
    if value.trim().is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(WorkspaceError::InvalidPathSegment(value.to_string()));
    }
    Ok(())
}

fn validate_artifact_relative_path(path: &Path) -> Result<(), WorkspaceError> {
    if path.is_absolute() {
        return Err(WorkspaceError::InvalidArtifactPath(path.to_path_buf()));
    }

    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(first)) if first == ARTIFACTS_DIR => {}
        _ => return Err(WorkspaceError::InvalidArtifactPath(path.to_path_buf())),
    }

    for component in components {
        match component {
            Component::Normal(_) => {}
            _ => return Err(WorkspaceError::InvalidArtifactPath(path.to_path_buf())),
        }
    }

    Ok(())
}

fn create_dir(path: impl AsRef<Path>) -> Result<(), WorkspaceError> {
    let path = path.as_ref();
    fs::create_dir_all(path).map_err(|source| WorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn canonicalize_existing_path(path: &Path) -> Result<PathBuf, WorkspaceError> {
    fs::canonicalize(path).map_err(|source| WorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut candidate = path;
    loop {
        if candidate.exists() {
            return candidate.to_path_buf();
        }
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => return path.to_path_buf(),
        }
    }
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<(), WorkspaceError> {
    let file = File::create(path).map_err(|source| WorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::to_writer_pretty(file, value).map_err(|source| WorkspaceError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, WorkspaceError> {
    let file = File::open(path).map_err(|source| WorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_reader(file).map_err(|source| WorkspaceError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn append_json_line<T: Serialize>(path: PathBuf, value: &T) -> Result<(), WorkspaceError> {
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })?;
    serde_json::to_writer(&mut file, value).map_err(|source| WorkspaceError::Json {
        path: path.clone(),
        source,
    })?;
    file.write_all(b"\n")
        .map_err(|source| WorkspaceError::Io { path, source })
}

fn read_json_lines<T: DeserializeOwned>(path: PathBuf) -> Result<Vec<T>, WorkspaceError> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(&path).map_err(|source| WorkspaceError::Io {
        path: path.clone(),
        source,
    })?;
    let reader = BufReader::new(file);
    let mut values = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        values.push(
            serde_json::from_str(&line).map_err(|source| WorkspaceError::Json {
                path: path.clone(),
                source,
            })?,
        );
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_visible_workspace_layout() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("case-001");
        let workspace = Workspace::init(
            &root,
            WorkspaceInitOptions {
                create_inputs: true,
            },
        )
        .unwrap();

        assert_eq!(workspace.root(), root.as_path());
        assert!(root.join(MANIFEST_FILE).is_file());
        assert!(root.join(ARTIFACTS_DIR).is_dir());
        assert!(root.join(ANALYSIS_DIR).is_dir());
        assert!(root.join(INPUTS_DIR).is_dir());

        let reopened = Workspace::open(&root).unwrap();
        assert_eq!(reopened.manifest().schema_version, 1);
    }

    #[test]
    fn register_and_list_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::init(temp.path().join("case-002"), Default::default()).unwrap();
        let artifact_path = workspace
            .allocate_artifact_path("raw", "command-output.txt")
            .unwrap();

        let metadata = ArtifactMetadata {
            artifact_id: ArtifactRef::new(Id::new("artifact-001").unwrap()),
            kind: "raw".to_string(),
            relative_path: artifact_path
                .strip_prefix(workspace.root())
                .unwrap()
                .to_path_buf(),
            created_at: Timestamp::now(),
            operation_id: None,
            byte_len: Some(0),
            description: Some("raw command output".to_string()),
        };

        workspace.register_artifact(&metadata).unwrap();
        let artifacts = workspace.list_artifacts().unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_id.id.as_str(), "artifact-001");
        let artifact = workspace
            .get_artifact(&ArtifactRef::new(Id::new("artifact-001").unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(artifact.kind, "raw");
    }

    #[test]
    fn creates_domain_artifact_directories() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            Workspace::init(temp.path().join("case-layout"), Default::default()).unwrap();
        let id = Id::new("session-001").unwrap();

        assert!(
            workspace
                .ensure_session_artifact_dir(&id)
                .unwrap()
                .ends_with(Path::new("artifacts").join("sessions").join("session-001"))
        );
        assert!(workspace.ensure_profile_artifact_dir(&id).unwrap().is_dir());
        assert!(
            workspace
                .ensure_ttd_recording_artifact_dir(&id)
                .unwrap()
                .is_dir()
        );
        assert!(
            workspace
                .ensure_reverse_session_artifact_dir(&id)
                .unwrap()
                .is_dir()
        );
    }

    #[test]
    fn rejects_artifact_paths_outside_workspace_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            Workspace::init(temp.path().join("case-paths"), Default::default()).unwrap();

        for path in [
            PathBuf::from(r"C:\absolute.txt"),
            PathBuf::from("../escape.txt"),
            PathBuf::from("analysis/report.md"),
            PathBuf::from("artifacts/../escape.txt"),
        ] {
            assert!(
                workspace.resolve_artifact_relative_path(&path).is_err(),
                "expected invalid artifact path: {}",
                path.display()
            );
        }
    }

    #[test]
    fn rejects_artifact_paths_through_directory_links_when_available() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            Workspace::init(temp.path().join("case-links"), Default::default()).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let link = workspace.root().join("artifacts").join("linked");

        if create_dir_link(&outside, &link).is_err() {
            return;
        }

        assert!(
            workspace
                .resolve_artifact_relative_path("artifacts/linked/output.txt")
                .is_err()
        );
    }

    #[test]
    fn appends_event_jsonl_and_text_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            Workspace::init(temp.path().join("case-events"), Default::default()).unwrap();

        workspace
            .append_event_jsonl(
                "artifacts/sessions/session-001/events.jsonl",
                &serde_json::json!({ "event": "created" }),
            )
            .unwrap();
        workspace
            .append_text_artifact("artifacts/sessions/session-001/transcript.log", "hello\n")
            .unwrap();

        let events = std::fs::read_to_string(
            workspace
                .root()
                .join("artifacts/sessions/session-001/events.jsonl"),
        )
        .unwrap();
        assert!(events.contains("created"));
        let transcript = std::fs::read_to_string(
            workspace
                .root()
                .join("artifacts/sessions/session-001/transcript.log"),
        )
        .unwrap();
        assert_eq!(transcript, "hello\n");
    }

    #[test]
    fn list_operations_accepts_legacy_pascal_case_status() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            Workspace::init(temp.path().join("case-legacy"), Default::default()).unwrap();
        std::fs::write(
            workspace.root().join(OPERATIONS_LOG),
            r#"{"operation_id":{"id":"op-legacy"},"adapter_id":"mock","capability":"native.version","status":"Success","created_at":{"unix_millis":1},"summary":"legacy success","artifacts":[]}"#,
        )
        .unwrap();

        let operations = workspace.list_operations().unwrap();
        assert_eq!(operations.len(), 1);
        assert!(matches!(operations[0].status, OperationStatus::Success));
        assert!(operations[0].raw_output.is_none());
    }

    #[test]
    fn command_audit_round_trips_and_facts_include_logs() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            Workspace::init(temp.path().join("case-audit"), Default::default()).unwrap();
        let operation_id = OperationRef::new(Id::new("op-audit").unwrap());
        let record = CommandAuditRecord {
            operation_id: operation_id.clone(),
            session_id: None,
            capability: "debug.eval".to_string(),
            command: ".echo hello".to_string(),
            created_at: Timestamp::now(),
            status: OperationStatus::Success,
            artifacts: Vec::new(),
            raw_output: None,
        };

        workspace.append_command_audit(&record).unwrap();

        let audit = workspace.list_command_audit().unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].operation_id, operation_id);
        let facts = workspace.facts().unwrap();
        assert_eq!(facts.command_audit.len(), 1);
    }

    #[cfg(windows)]
    fn create_dir_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[cfg(unix)]
    fn create_dir_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(not(any(windows, unix)))]
    fn create_dir_link(_target: &Path, _link: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "directory links are not supported on this platform",
        ))
    }
}
