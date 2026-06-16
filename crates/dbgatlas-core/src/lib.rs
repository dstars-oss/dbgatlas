use dbgatlas_adapter::{
    Adapter, AdapterError, AdapterId, ArtifactOutput, Capability, Invocation, InvocationResult,
    InvocationStatus, ensure_capability,
};
use dbgatlas_model::{Id, OperationRef, Timestamp};
use dbgatlas_workspace::{
    ArtifactMetadata, OperationRecord, OperationStatus, Workspace, WorkspaceError,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("adapter `{0}` is not registered")]
    AdapterNotRegistered(String),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}

pub struct Core {
    workspace: Workspace,
    adapters: HashMap<AdapterId, Arc<dyn Adapter>>,
}

impl Core {
    pub fn open_workspace(root: impl Into<PathBuf>) -> Result<Self, CoreError> {
        Ok(Self {
            workspace: Workspace::open(root)?,
            adapters: HashMap::new(),
        })
    }

    pub fn with_workspace(workspace: Workspace) -> Self {
        Self {
            workspace,
            adapters: HashMap::new(),
        }
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn register_adapter<A>(&mut self, adapter: A)
    where
        A: Adapter + 'static,
    {
        let id = adapter.metadata().id;
        self.adapters.insert(id, Arc::new(adapter));
    }

    pub fn invoke(
        &self,
        adapter_id: &AdapterId,
        capability: &Capability,
        params: Value,
    ) -> Result<InvocationResult, CoreError> {
        let adapter = self
            .adapters
            .get(adapter_id)
            .ok_or_else(|| CoreError::AdapterNotRegistered(adapter_id.as_str().to_string()))?;

        let operation_id = next_operation_ref();
        let invocation = Invocation {
            operation_id: operation_id.clone(),
            adapter_id: adapter_id.clone(),
            capability: capability.clone(),
            params,
            workspace_root: Some(self.workspace.root().to_path_buf()),
            artifact_output_dir: Some(
                self.workspace
                    .root()
                    .join(dbgatlas_workspace::ARTIFACTS_DIR),
            ),
        };

        if let Err(error) = ensure_capability(adapter.as_ref(), capability) {
            self.append_operation(
                operation_id,
                adapter_id,
                capability,
                OperationStatus::Failed,
                error.to_string(),
                Vec::new(),
            )?;
            return Err(CoreError::Adapter(error));
        }

        match adapter.invoke(invocation) {
            Ok(result) => {
                let artifact_refs = self.register_artifacts(operation_id.clone(), &result)?;
                let status = match result.status {
                    InvocationStatus::Success => OperationStatus::Success,
                    InvocationStatus::Failed => OperationStatus::Failed,
                };
                self.append_operation(
                    operation_id,
                    adapter_id,
                    capability,
                    status,
                    result.summary.clone(),
                    artifact_refs,
                )?;
                Ok(result)
            }
            Err(error) => {
                self.append_operation(
                    operation_id,
                    adapter_id,
                    capability,
                    OperationStatus::Failed,
                    error.to_string(),
                    Vec::new(),
                )?;
                Err(CoreError::Adapter(error))
            }
        }
    }

    pub fn start_operation(
        &self,
        adapter_id: &AdapterId,
        capability: &Capability,
        summary: impl Into<String>,
    ) -> Result<OperationRef, CoreError> {
        let operation_id = next_operation_ref();
        self.append_operation(
            operation_id.clone(),
            adapter_id,
            capability,
            OperationStatus::Running,
            summary.into(),
            Vec::new(),
        )?;
        Ok(operation_id)
    }

    pub fn finish_operation(
        &self,
        operation_id: OperationRef,
        adapter_id: &AdapterId,
        capability: &Capability,
        summary: impl Into<String>,
        artifacts: Vec<dbgatlas_model::ArtifactRef>,
    ) -> Result<(), CoreError> {
        self.append_operation(
            operation_id,
            adapter_id,
            capability,
            OperationStatus::Success,
            summary.into(),
            artifacts,
        )
    }

    pub fn fail_operation(
        &self,
        operation_id: OperationRef,
        adapter_id: &AdapterId,
        capability: &Capability,
        summary: impl Into<String>,
        artifacts: Vec<dbgatlas_model::ArtifactRef>,
    ) -> Result<(), CoreError> {
        self.append_operation(
            operation_id,
            adapter_id,
            capability,
            OperationStatus::Failed,
            summary.into(),
            artifacts,
        )
    }

    pub fn cancel_operation(
        &self,
        operation_id: OperationRef,
        adapter_id: &AdapterId,
        capability: &Capability,
        summary: impl Into<String>,
    ) -> Result<(), CoreError> {
        self.append_operation(
            operation_id,
            adapter_id,
            capability,
            OperationStatus::Canceled,
            summary.into(),
            Vec::new(),
        )
    }

    fn register_artifacts(
        &self,
        operation_id: OperationRef,
        result: &InvocationResult,
    ) -> Result<Vec<dbgatlas_model::ArtifactRef>, CoreError> {
        for artifact in &result.created_artifacts {
            self.register_artifact_output(operation_id.clone(), artifact)?;
        }
        if let Some(raw_output) = &result.raw_output {
            self.register_artifact_output(operation_id, raw_output)?;
        }
        Ok(result.artifact_refs())
    }

    fn register_artifact_output(
        &self,
        operation_id: OperationRef,
        artifact: &ArtifactOutput,
    ) -> Result<(), CoreError> {
        let metadata = ArtifactMetadata {
            artifact_id: artifact.artifact_id.clone(),
            kind: artifact.kind.clone(),
            relative_path: artifact.relative_path.clone(),
            created_at: Timestamp::now(),
            operation_id: Some(operation_id),
            byte_len: None,
            description: artifact.description.clone(),
        };
        self.workspace.register_artifact(&metadata)?;
        Ok(())
    }

    fn append_operation(
        &self,
        operation_id: OperationRef,
        adapter_id: &AdapterId,
        capability: &Capability,
        status: OperationStatus,
        summary: String,
        artifacts: Vec<dbgatlas_model::ArtifactRef>,
    ) -> Result<(), CoreError> {
        let record = OperationRecord {
            operation_id,
            adapter_id: adapter_id.as_str().to_string(),
            capability: capability.as_str().to_string(),
            status,
            created_at: Timestamp::now(),
            summary,
            artifacts,
            raw_output: None,
        };
        self.workspace.append_operation(&record)?;
        Ok(())
    }
}

fn next_operation_ref() -> OperationRef {
    let count = OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("op-{}-{}", Timestamp::now().unix_millis, count);
    OperationRef::new(Id::new(id).expect("generated operation ids are valid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgatlas_adapter::{AdapterMetadata, ArtifactOutput, InvocationStatus};
    use dbgatlas_model::ArtifactRef;
    use dbgatlas_workspace::WorkspaceInitOptions;
    use serde_json::json;

    struct MockAdapter {
        result_status: InvocationStatus,
        artifact_output: bool,
    }

    impl Adapter for MockAdapter {
        fn metadata(&self) -> AdapterMetadata {
            AdapterMetadata {
                id: AdapterId::new("mock"),
                display_name: "Mock Adapter".to_string(),
                version: "0.1.0".to_string(),
            }
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability::new("native.version")]
        }

        fn invoke(&self, _invocation: Invocation) -> Result<InvocationResult, AdapterError> {
            let mut result = InvocationResult {
                status: self.result_status.clone(),
                summary: "mock invocation complete".to_string(),
                payload: json!({ "version": "0.1.0" }),
                created_artifacts: Vec::new(),
                raw_output: None,
            };
            if self.artifact_output {
                let mut artifact = ArtifactOutput::new(
                    ArtifactRef::new(Id::new("artifact-001").unwrap()),
                    "raw",
                    "artifacts/raw/output.txt",
                );
                artifact.description = Some("raw output".to_string());
                result.raw_output = Some(artifact);
            }
            Ok(result)
        }
    }

    #[test]
    fn invokes_adapter_and_records_operation() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::init(
            temp.path().join("case-core"),
            WorkspaceInitOptions::default(),
        )
        .unwrap();
        let mut core = Core::with_workspace(workspace);
        core.register_adapter(MockAdapter {
            result_status: InvocationStatus::Success,
            artifact_output: false,
        });

        let result = core
            .invoke(
                &AdapterId::new("mock"),
                &Capability::new("native.version"),
                json!({}),
            )
            .unwrap();

        assert!(matches!(result.status, InvocationStatus::Success));
        let operations = core.workspace().list_operations().unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].adapter_id, "mock");
    }

    #[test]
    fn records_structured_failed_result_as_failed_operation() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::init(
            temp.path().join("case-failed"),
            WorkspaceInitOptions::default(),
        )
        .unwrap();
        let mut core = Core::with_workspace(workspace);
        core.register_adapter(MockAdapter {
            result_status: InvocationStatus::Failed,
            artifact_output: false,
        });

        let result = core
            .invoke(
                &AdapterId::new("mock"),
                &Capability::new("native.version"),
                json!({}),
            )
            .unwrap();

        assert!(matches!(result.status, InvocationStatus::Failed));
        let operations = core.workspace().list_operations().unwrap();
        assert!(matches!(operations[0].status, OperationStatus::Failed));
    }

    #[test]
    fn registers_raw_output_artifact_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::init(
            temp.path().join("case-artifact"),
            WorkspaceInitOptions::default(),
        )
        .unwrap();
        let mut core = Core::with_workspace(workspace);
        core.register_adapter(MockAdapter {
            result_status: InvocationStatus::Success,
            artifact_output: true,
        });

        core.invoke(
            &AdapterId::new("mock"),
            &Capability::new("native.version"),
            json!({}),
        )
        .unwrap();

        let artifacts = core.workspace().list_artifacts().unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_id.id.as_str(), "artifact-001");

        let operations = core.workspace().list_operations().unwrap();
        assert_eq!(operations[0].artifacts.len(), 1);
    }

    #[test]
    fn records_long_running_operation_status_transitions() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::init(
            temp.path().join("case-long-op"),
            WorkspaceInitOptions::default(),
        )
        .unwrap();
        let core = Core::with_workspace(workspace);
        let adapter_id = AdapterId::new("mock");
        let capability = Capability::new("debug.eval");

        let operation_id = core
            .start_operation(&adapter_id, &capability, "debug command running")
            .unwrap();
        core.cancel_operation(
            operation_id.clone(),
            &adapter_id,
            &capability,
            "debug command canceled",
        )
        .unwrap();

        let operations = core.workspace().list_operations().unwrap();
        assert_eq!(operations.len(), 2);
        assert_eq!(operations[0].operation_id.id, operation_id.id);
        assert!(matches!(operations[0].status, OperationStatus::Running));
        assert!(matches!(operations[1].status, OperationStatus::Canceled));
    }
}
