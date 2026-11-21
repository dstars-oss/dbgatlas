mod history;
pub use history::{HistoryFilter, HistoryStore, OperationPage};
static JSONL_IO_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
use dbgatlas_model::{ArtifactRef, Id, InvalidId, OperationRef, Timestamp};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub const MANIFEST_FILE: &str = "dbgatlas-workspace.json";
pub const ARTIFACTS_DIR: &str = "artifacts";
pub const ANALYSIS_DIR: &str = "analysis";
pub const INPUTS_DIR: &str = "inputs";
pub const SESSIONS_DIR: &str = "sessions";
pub const RECORDINGS_DIR: &str = "recordings";
pub const PROFILES_DIR: &str = "profiles";
pub const TTD_RECORDINGS_DIR: &str = "ttd_recordings";

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
    #[error("history database: {0}")]
    History(String),
    #[error("history database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("history record: {0}")]
    Record(#[from] serde_json::Error),
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
    #[serde(default)]
    pub producer_session_id: Option<dbgatlas_model::SessionRef>,
    #[serde(default)]
    pub producer_execution_id: Option<OperationRef>,
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
    #[serde(default, flatten)]
    pub details: dbgatlas_model::OperationDetails,
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

pub use dbgatlas_model::OperationState as OperationStatus;

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    manifest: WorkspaceManifest,
    history: HistoryStore,
}

impl Workspace {
    pub fn init(
        root: impl Into<PathBuf>,
        options: WorkspaceInitOptions,
    ) -> Result<Self, WorkspaceError> {
        HistoryStore::local().init_workspace(root.into(), options)
    }

    pub fn open(root: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        HistoryStore::local().open_workspace(root.into())
    }

    pub fn load_manifest(root: &Path) -> Result<WorkspaceManifest, WorkspaceError> {
        let path = root.join(MANIFEST_FILE);
        if !path.try_exists().map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })? {
            return Err(WorkspaceError::ManifestNotFound(path));
        }
        let manifest: WorkspaceManifest = read_json(&path)?;
        if manifest.schema_version != 2 {
            return Err(WorkspaceError::History(format!(
                "unsupported workspace format {} at {}; use a new workspace",
                manifest.schema_version,
                root.display()
            )));
        }
        Ok(manifest)
    }

    /// File-only worker handle. Recording workers never read or write catalog metadata.
    pub fn open_files(root: PathBuf) -> Result<Self, WorkspaceError> {
        let manifest = Self::load_manifest(&root)?;
        Ok(Self {
            root,
            manifest,
            history: HistoryStore::local(),
        })
    }

    pub fn verify_files(&self) -> Result<(), WorkspaceError> {
        if Self::load_manifest(&self.root)?.workspace_id != self.manifest.workspace_id {
            return Err(WorkspaceError::History(
                "project directory now contains a different workspace".into(),
            ));
        }
        Ok(())
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
        self.verify_files()?;
        validate_path_segment(kind)?;
        validate_path_segment(file_name)?;
        let dir = self.root.join(ARTIFACTS_DIR).join(kind);
        create_dir(&dir)?;
        Ok(dir.join(file_name))
    }

    pub fn ensure_session_artifact_dir(&self, session_id: &Id) -> Result<PathBuf, WorkspaceError> {
        self.ensure_domain_artifact_dir(SESSIONS_DIR, session_id)
    }

    pub fn ensure_recording_artifact_dir(
        &self,
        recording_id: &Id,
    ) -> Result<PathBuf, WorkspaceError> {
        self.ensure_domain_artifact_dir(RECORDINGS_DIR, recording_id)
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

    pub fn resolve_artifact_relative_path(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<PathBuf, WorkspaceError> {
        self.verify_files()?;
        let relative_path = relative_path.as_ref();
        validate_artifact_relative_path(relative_path)?;
        let path = self.root.join(relative_path);
        self.ensure_resolved_artifact_path_is_contained(&path)?;
        Ok(path)
    }

    pub fn register_artifact(&self, metadata: &ArtifactMetadata) -> Result<(), WorkspaceError> {
        self.resolve_artifact_relative_path(&metadata.relative_path)?;
        let mut metadata = metadata.clone();
        if let Some(id) = &metadata.operation_id {
            if let Some(operation) = self.get_operation(id)? {
                if let Some(session) = operation.details.session_id {
                    if metadata
                        .producer_session_id
                        .as_ref()
                        .is_some_and(|producer| producer != &session)
                    {
                        return Err(WorkspaceError::InvalidArtifactPath(metadata.relative_path));
                    }
                    metadata.producer_session_id = Some(session);
                    if metadata
                        .producer_execution_id
                        .as_ref()
                        .is_some_and(|producer| producer != id)
                    {
                        return Err(WorkspaceError::InvalidArtifactPath(metadata.relative_path));
                    }
                    metadata.producer_execution_id = Some(id.clone());
                }
            }
        }
        self.save_artifact(&metadata)
    }

    /// Resolve the registered relative path, then validate the opened object too.
    /// A junction swapped between lookup and open cannot expose an outside file.
    pub fn open_artifact_read(&self, relative: impl AsRef<Path>) -> Result<File, WorkspaceError> {
        let path = self.resolve_artifact_relative_path(relative)?;
        let file = File::open(&path).map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })?;
        #[cfg(windows)]
        {
            use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
            use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
            let mut buffer = vec![0u16; 32768];
            let size = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle() as _,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    0,
                )
            };
            if size == 0 || size as usize >= buffer.len() {
                return Err(WorkspaceError::Io {
                    path,
                    source: std::io::Error::last_os_error(),
                });
            }
            let final_path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..size as usize]));
            let root = fs::canonicalize(self.root.join(ARTIFACTS_DIR)).map_err(|source| {
                WorkspaceError::Io {
                    path: path.clone(),
                    source,
                }
            })?;
            let root = root.to_string_lossy().to_lowercase();
            let opened = final_path.to_string_lossy().to_lowercase();
            if !opened.starts_with(&(root.trim_end_matches('\\').to_string() + "\\")) {
                return Err(WorkspaceError::InvalidArtifactPath(path));
            }
        }
        Ok(file)
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
        self.verify_files()?;
        let dir = self.root.join(ARTIFACTS_DIR).join(domain).join(id.as_str());
        create_dir(&dir)?;
        Ok(dir)
    }

    fn ensure_resolved_artifact_path_is_contained(
        &self,
        path: &Path,
    ) -> Result<(), WorkspaceError> {
        // artifact 路径必须停留在 workspace 的 artifacts/ 事实层里。
        // 这里先规范化已存在的根目录，再规范化目标路径最近的已存在祖先，
        // 这样能挡住 `artifacts/linked/...` 这类 symlink/junction 逃逸。
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

fn merge_update(record: &mut OperationRecord, previous: OperationRecord) {
    if matches!(
        previous.status,
        OperationStatus::Success | OperationStatus::Failed | OperationStatus::Canceled
    ) && matches!(
        record.status,
        OperationStatus::Pending | OperationStatus::Running | OperationStatus::Unknown
    ) {
        record.status = previous.status.clone();
        record.summary = previous.summary.clone();
        record.artifacts = previous.artifacts.clone();
        record.raw_output = previous.raw_output.clone();
    }
    record.created_at = previous.created_at;
    record.details.session_id = previous
        .details
        .session_id
        .or(record.details.session_id.take());
    record.details.input = previous.details.input.or(record.details.input.take());
    record.details.execution_context = previous
        .details
        .execution_context
        .or(record.details.execution_context.take());
    if record.details.related_operation_id.is_none() {
        record.details.related_operation_id = previous.details.related_operation_id;
    }
    if record.details.output_error.is_none() {
        record.details.output_error = previous.details.output_error;
    }
    record.details.update_seq = previous.details.update_seq + 1;
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
    let _guard = JSONL_IO_LOCK.lock().map_err(|_| WorkspaceError::Io {
        path: path.clone(),
        source: std::io::Error::other("JSONL lock poisoned"),
    })?;
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }

    // workspace 的事实索引采用 append-only JSONL：便于人工审计，也避免
    // 工具运行中途失败时重写整份索引造成更大的损坏面。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_merges_updates_without_inventing_identity_or_changing_input() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = HistoryStore::new(temp.path().join("history.sqlite3"))
            .init_workspace(temp.path().join("legacy"), Default::default())
            .unwrap();
        let legacy = serde_json::json!({"operation_id":{"id":"old-op"},"adapter_id":"service","capability":"debug.eval",
            "status":"Running","created_at":{"unix_millis":1},"summary":"old","artifacts":[],"raw_output":null});
        workspace
            .append_operation(&serde_json::from_value(legacy).unwrap())
            .unwrap();
        let id = OperationRef::new(Id::new("old-op").unwrap());
        let mut record = workspace.get_operation(&id).unwrap().unwrap();
        assert!(record.details.session_id.is_none());
        assert!(record.details.execution_context.is_none());
        record.status = OperationStatus::Unknown;
        record.details.input = Some(serde_json::json!({"command":"original"}));
        workspace.append_operation(&record).unwrap();
        record.details.input = Some(serde_json::json!({"command":"replacement"}));
        workspace.append_operation(&record).unwrap();
        let merged = workspace.list_operations().unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].details.update_seq, 2);
        assert_eq!(
            merged[0].details.input.as_ref().unwrap()["command"],
            "original"
        );
        assert_eq!(merged[0].status, OperationStatus::Unknown);
        assert_eq!(
            workspace
                .history_entries(Some(id.id.as_str()), None, 0, 10)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn init_creates_visible_workspace_layout() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("case-001");
        let workspace = HistoryStore::new(temp.path().join("history.sqlite3"))
            .init_workspace(
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

        let reopened = HistoryStore::new(temp.path().join("history.sqlite3"))
            .open_workspace(&root)
            .unwrap();
        assert_eq!(reopened.manifest().schema_version, 2);
    }

    #[test]
    fn register_and_list_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = HistoryStore::new(temp.path().join("history.sqlite3"))
            .init_workspace(temp.path().join("case-002"), Default::default())
            .unwrap();
        let artifact_path = workspace
            .allocate_artifact_path("raw", "command-output.txt")
            .unwrap();

        let metadata = ArtifactMetadata {
            producer_session_id: None,
            producer_execution_id: None,
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
        let workspace = HistoryStore::new(temp.path().join("history.sqlite3"))
            .init_workspace(temp.path().join("case-layout"), Default::default())
            .unwrap();
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
                .ensure_recording_artifact_dir(&id)
                .unwrap()
                .is_dir()
        );
        assert!(
            workspace
                .ensure_ttd_recording_artifact_dir(&id)
                .unwrap()
                .is_dir()
        );
    }

    #[test]
    fn rejects_artifact_paths_outside_workspace_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = HistoryStore::new(temp.path().join("history.sqlite3"))
            .init_workspace(temp.path().join("case-paths"), Default::default())
            .unwrap();

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
        let workspace = HistoryStore::new(temp.path().join("history.sqlite3"))
            .init_workspace(temp.path().join("case-links"), Default::default())
            .unwrap();
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
        let workspace = HistoryStore::new(temp.path().join("history.sqlite3"))
            .init_workspace(temp.path().join("case-events"), Default::default())
            .unwrap();

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
    fn rejects_old_manifest_without_modifying_it() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = temp.path().join(MANIFEST_FILE);
        let old = r#"{"schema_version":1,"workspace_id":"old","created_at":{"unix_millis":1},"tool":{"name":"DbgAtlas","version":"0.4.0"}}"#;
        fs::write(&manifest, old).unwrap();
        assert!(
            Workspace::load_manifest(temp.path())
                .unwrap_err()
                .to_string()
                .contains("unsupported workspace format")
        );
        assert_eq!(fs::read_to_string(manifest).unwrap(), old);
    }

    #[test]
    fn command_audit_round_trips_and_facts_include_logs() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = HistoryStore::new(temp.path().join("history.sqlite3"))
            .init_workspace(temp.path().join("case-audit"), Default::default())
            .unwrap();
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

        assert!(workspace.append_command_audit(&record).is_err());
        let execution = serde_json::json!({"operation_id":operation_id,"adapter_id":"dbgeng","capability":"debug.eval","status":"success","created_at":{"unix_millis":1},"summary":"done","artifacts":[]});
        workspace
            .append_operation(&serde_json::from_value(execution).unwrap())
            .unwrap();
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
