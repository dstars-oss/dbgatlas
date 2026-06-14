use dbgatlas_model::{ArtifactRef, OperationRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AdapterId(String);

impl AdapterId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capability(String);

impl Capability {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdapterMetadata {
    pub id: AdapterId,
    pub display_name: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub operation_id: OperationRef,
    pub adapter_id: AdapterId,
    pub capability: Capability,
    pub params: Value,
    pub workspace_root: Option<PathBuf>,
    pub artifact_output_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvocationResult {
    pub status: InvocationStatus,
    pub summary: String,
    pub payload: Value,
    pub created_artifacts: Vec<ArtifactOutput>,
    pub raw_output: Option<ArtifactOutput>,
}

impl InvocationResult {
    pub fn success(summary: impl Into<String>, payload: Value) -> Self {
        Self {
            status: InvocationStatus::Success,
            summary: summary.into(),
            payload,
            created_artifacts: Vec::new(),
            raw_output: None,
        }
    }

    pub fn artifact_refs(&self) -> Vec<ArtifactRef> {
        let mut refs: Vec<_> = self
            .created_artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect();
        if let Some(raw_output) = &self.raw_output {
            if !refs
                .iter()
                .any(|artifact| artifact == &raw_output.artifact_id)
            {
                refs.push(raw_output.artifact_id.clone());
            }
        }
        refs
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationStatus {
    Success,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactOutput {
    pub artifact_id: ArtifactRef,
    pub kind: String,
    pub relative_path: PathBuf,
    pub description: Option<String>,
}

impl ArtifactOutput {
    pub fn new(
        artifact_id: ArtifactRef,
        kind: impl Into<String>,
        relative_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            artifact_id,
            kind: kind.into(),
            relative_path: relative_path.into(),
            description: None,
        }
    }
}

pub trait Adapter: Send + Sync {
    fn metadata(&self) -> AdapterMetadata;
    fn capabilities(&self) -> Vec<Capability>;
    fn invoke(&self, invocation: Invocation) -> Result<InvocationResult, AdapterError>;

    fn supports(&self, capability: &Capability) -> bool {
        self.capabilities()
            .iter()
            .any(|candidate| candidate == capability)
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("invalid adapter input: {0}")]
    InvalidInput(String),
    #[error("unsupported capability `{capability}` for adapter `{adapter}`")]
    UnsupportedCapability { adapter: String, capability: String },
    #[error("native adapter failure: {0}")]
    NativeFailure(String),
    #[error("io failure: {0}")]
    Io(String),
    #[error("internal adapter failure: {0}")]
    Internal(String),
}

pub fn ensure_capability(
    adapter: &dyn Adapter,
    capability: &Capability,
) -> Result<(), AdapterError> {
    if adapter.supports(capability) {
        return Ok(());
    }

    let metadata = adapter.metadata();
    Err(AdapterError::UnsupportedCapability {
        adapter: metadata.id.as_str().to_string(),
        capability: capability.as_str().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgatlas_model::{Id, OperationRef};
    use serde_json::json;

    struct MockAdapter;

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

        fn invoke(&self, invocation: Invocation) -> Result<InvocationResult, AdapterError> {
            ensure_capability(self, &invocation.capability)?;
            Ok(InvocationResult::success("ok", json!({ "mock": true })))
        }
    }

    #[test]
    fn reports_supported_capabilities() {
        let adapter = MockAdapter;
        assert!(adapter.supports(&Capability::new("native.version")));
        assert!(!adapter.supports(&Capability::new("debug.attach")));
    }

    #[test]
    fn rejects_unsupported_capability() {
        let adapter = MockAdapter;
        let invocation = Invocation {
            operation_id: OperationRef::new(Id::new("op-001").unwrap()),
            adapter_id: AdapterId::new("mock"),
            capability: Capability::new("debug.attach"),
            params: json!({}),
            workspace_root: None,
            artifact_output_dir: None,
        };

        let error = adapter.invoke(invocation).unwrap_err();
        assert!(matches!(error, AdapterError::UnsupportedCapability { .. }));
    }
}
