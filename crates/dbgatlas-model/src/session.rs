//! Serializable session facts. Native handles and transport details belong to the executor.
use crate::{ArtifactRef, Id, OperationRef, SessionRef, Timestamp, WorkspaceRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Debug,
    Recording,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    Opening,
    Active,
    Closing,
    Closed,
    Failed,
}
impl SessionLifecycle {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Connected,
    Disconnected,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    Unknown,
    Running,
    Stopped,
    PartiallyStopped,
    Exited,
    Offline,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub system_instance: Option<String>,
    pub os: Option<String>,
    pub architecture: Option<String>,
}
impl Environment {
    /// The local execution-environment alias; boot and service identities remain separate.
    pub fn local(_service_instance: &str) -> Self {
        Self {
            id: "local".into(),
            system_instance: None,
            os: Some(std::env::consts::OS.into()),
            architecture: Some(std::env::consts::ARCH.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentPath {
    pub environment_id: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInstance {
    pub environment_id: String,
    pub pid: u32,
    /// Windows FILETIME, in 100 ns ticks. Never compare this with a Unix timestamp.
    #[serde(default, with = "optional_u64_string")]
    pub creation_time_100ns: Option<u64>,
}
mod optional_u64_string {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        value.map(|n| n.to_string()).serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Evidence {
            Decimal(String),
            Legacy(u64),
        }
        match Option::<Evidence>::deserialize(deserializer)? {
            None => Ok(None),
            Some(Evidence::Legacy(n)) => Ok(Some(n)),
            Some(Evidence::Decimal(text)) => {
                text.parse().map(Some).map_err(serde::de::Error::custom)
            }
        }
    }
}

impl ProcessInstance {
    pub fn same_instance(&self, other: &Self) -> bool {
        self.environment_id == other.environment_id
            && self.pid == other.pid
            && self.creation_time_100ns.is_some()
            && self.creation_time_100ns == other.creation_time_100ns
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOwnership {
    Managed,
    External,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerBinding {
    pub worker_id: Id,
    pub environment: Environment,
    pub process: Option<ProcessInstance>,
    pub backend: String,
    pub execution_identity: String,
    pub ownership: ProcessOwnership,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetBinding {
    pub revision: u64,
    pub kind: String,
    pub environment: Option<Environment>,
    pub process: Option<ProcessInstance>,
    pub location: Option<EnvironmentPath>,
    pub open_request: Value,
    pub created_by_session: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateObservation {
    pub state: TargetState,
    pub observed_at: Timestamp,
    pub source: String,
    pub last_known: Option<TargetState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosePolicy {
    pub normal: String,
    pub abnormal: String,
}
impl Default for ClosePolicy {
    fn default() -> Self {
        Self {
            normal: "preserve_target".into(),
            abnormal: "terminate_verified_owned_processes".into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupResult {
    pub complete: bool,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub input_artifacts: Vec<ArtifactRef>,
    pub schema_version: u32,
    pub session_id: SessionRef,
    pub kind: SessionKind,
    pub workspace: WorkspaceRef,
    pub service_instance: String,
    pub control_environment: Environment,
    pub lifecycle: SessionLifecycle,
    pub connection: ConnectionState,
    pub worker: Option<WorkerBinding>,
    pub target: TargetBinding,
    pub target_state: StateObservation,
    pub context: Value,
    pub capabilities: Vec<String>,
    pub policy: ClosePolicy,
    pub cleanup: CleanupResult,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub last_operation_id: Option<OperationRef>,
    pub artifacts: Vec<ArtifactRef>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionContext {
    pub worker: Option<WorkerBinding>,
    pub target: TargetBinding,
    pub context: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OperationDetails {
    #[serde(default)]
    pub session_id: Option<SessionRef>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub execution_context: Option<ExecutionContext>,
    #[serde(default)]
    pub update_seq: u64,
    #[serde(default)]
    pub output_error: Option<String>,
    #[serde(default)]
    pub related_operation_id: Option<OperationRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    #[serde(alias = "Running")]
    Running,
    #[serde(alias = "Success")]
    Success,
    #[serde(alias = "Failed")]
    Failed,
    #[serde(alias = "Canceled")]
    Canceled,
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_time_is_lossless_in_javascript_and_reads_legacy_numbers() {
        let value = ProcessInstance {
            environment_id: "local".into(),
            pid: 1,
            creation_time_100ns: Some(134391239094521174),
        };
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["creation_time_100ns"], "134391239094521174");
        assert_eq!(
            serde_json::from_value::<ProcessInstance>(json).unwrap(),
            value
        );
        let legacy = serde_json::json!({"environment_id":"local","pid":1,"creation_time_100ns":134391239094521174u64});
        assert_eq!(
            serde_json::from_value::<ProcessInstance>(legacy).unwrap(),
            value
        );
    }

    #[test]
    fn unknown_creation_time_never_proves_process_identity() {
        let a = ProcessInstance {
            environment_id: "a".into(),
            pid: 7,
            creation_time_100ns: None,
        };
        assert!(!a.same_instance(&a));
        let b = ProcessInstance {
            creation_time_100ns: Some(10),
            ..a.clone()
        };
        assert!(b.same_instance(&b));
        assert!(!b.same_instance(&ProcessInstance {
            creation_time_100ns: Some(11),
            ..b.clone()
        }));
        assert!(!b.same_instance(&ProcessInstance {
            environment_id: "b".into(),
            ..b.clone()
        }));
    }
    #[test]
    fn remote_paths_are_preserved_without_host_interpretation() {
        let path = EnvironmentPath {
            environment_id: "linux".into(),
            path: "/var/dumps/core".into(),
        };
        let encoded = serde_json::to_string(&path).unwrap();
        assert_eq!(
            serde_json::from_str::<EnvironmentPath>(&encoded).unwrap(),
            path
        );
    }
}
