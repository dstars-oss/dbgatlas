use dbgatlas_model::{ArtifactRef, OperationRef, SessionRef, Timestamp};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DebugError {
    #[error("debug target path must not be empty")]
    EmptyPath,
    #[error("debug target path contains a NUL byte")]
    PathContainsNul,
    #[error("attach pid must be greater than zero")]
    InvalidPid,
    #[error("refusing to attach to the current DbgAtlas process")]
    AttachCurrentProcess,
    #[error("launch argument contains a NUL byte")]
    ArgumentContainsNul,
    #[error("debug command must not be empty")]
    EmptyCommand,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DebugTarget {
    Dump {
        path: PathBuf,
    },
    Attach {
        pid: u32,
    },
    Launch {
        executable: PathBuf,
        args: Vec<String>,
    },
}

impl DebugTarget {
    pub fn validate(self) -> Result<Self, DebugError> {
        match self {
            Self::Dump { path } => validate_path(&path).map(|path| Self::Dump { path }),
            Self::Attach { pid } => validate_attach_pid(pid),
            Self::Launch { executable, args } => validate_launch_target(&executable, args),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugSessionState {
    Created,
    Starting,
    Ready,
    Break,
    Running,
    Closing,
    Closed,
    Error,
}

impl DebugSessionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Error)
    }

    pub fn is_reusable(self) -> bool {
        matches!(self, Self::Ready | Self::Break)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugSession {
    pub id: SessionRef,
    pub target: DebugTarget,
    pub state: DebugSessionState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub current_operation: Option<OperationRef>,
    pub last_operation: Option<OperationRef>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateDebugSession {
    pub target: DebugTarget,
    pub startup_timeout_ms: Option<u64>,
}

impl CreateDebugSession {
    pub fn validate(self) -> Result<Self, DebugError> {
        Ok(Self {
            target: self.target.validate()?,
            startup_timeout_ms: self.startup_timeout_ms,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalDebugCommand {
    pub session_id: SessionRef,
    pub command: String,
    pub timeout_ms: Option<u64>,
}

impl EvalDebugCommand {
    pub fn validate(&self) -> Result<(), DebugError> {
        validate_command(&self.command)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugCommandResult {
    pub session_id: SessionRef,
    pub operation_id: Option<OperationRef>,
    pub command: String,
    pub output: String,
    pub final_state: Option<DebugSessionState>,
    pub raw_output: Option<ArtifactRef>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

pub trait DebugSessionManager: Send + Sync {
    fn create_session(&self, request: CreateDebugSession) -> Result<DebugSession, DebugError>;
    fn get_session(&self, session_id: &SessionRef) -> Result<DebugSession, DebugError>;
    fn list_sessions(&self) -> Result<Vec<DebugSession>, DebugError>;
    fn close_session(&self, session_id: &SessionRef) -> Result<DebugSession, DebugError>;
    fn eval(&self, request: EvalDebugCommand) -> Result<DebugCommandResult, DebugError>;
}

pub fn validate_command(command: &str) -> Result<(), DebugError> {
    if command.trim().is_empty() {
        return Err(DebugError::EmptyCommand);
    }
    Ok(())
}

fn validate_path(path: &Path) -> Result<PathBuf, DebugError> {
    if path.as_os_str().is_empty() {
        return Err(DebugError::EmptyPath);
    }
    let text = path.as_os_str().to_string_lossy();
    if text.contains('\0') {
        return Err(DebugError::PathContainsNul);
    }
    Ok(path.to_path_buf())
}

fn validate_attach_pid(pid: u32) -> Result<DebugTarget, DebugError> {
    if pid == 0 {
        return Err(DebugError::InvalidPid);
    }
    if pid == std::process::id() {
        return Err(DebugError::AttachCurrentProcess);
    }
    Ok(DebugTarget::Attach { pid })
}

fn validate_launch_target(executable: &Path, args: Vec<String>) -> Result<DebugTarget, DebugError> {
    let executable = validate_path(executable)?;
    if args.iter().any(|arg| arg.contains('\0')) {
        return Err(DebugError::ArgumentContainsNul);
    }
    Ok(DebugTarget::Launch { executable, args })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgatlas_model::{Id, SessionRef};

    #[test]
    fn session_state_classifies_terminal_and_reusable_states() {
        assert!(DebugSessionState::Closed.is_terminal());
        assert!(DebugSessionState::Error.is_terminal());
        assert!(!DebugSessionState::Break.is_terminal());
        assert!(DebugSessionState::Ready.is_reusable());
        assert!(DebugSessionState::Break.is_reusable());
        assert!(!DebugSessionState::Running.is_reusable());
        assert!(!DebugSessionState::Starting.is_reusable());
        assert!(!DebugSessionState::Closed.is_reusable());
    }

    #[test]
    fn target_round_trips_through_json() {
        let target = DebugTarget::Launch {
            executable: PathBuf::from(r"C:\app\sample.exe"),
            args: vec!["--flag".to_string()],
        };
        let json = serde_json::to_string(&target).unwrap();
        let restored: DebugTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, target);
    }

    #[test]
    fn rejects_empty_debug_command() {
        let request = EvalDebugCommand {
            session_id: SessionRef::new(Id::new("session-001").unwrap()),
            command: "  ".to_string(),
            timeout_ms: None,
        };
        assert!(matches!(request.validate(), Err(DebugError::EmptyCommand)));
    }

    #[test]
    fn rejects_invalid_targets() {
        assert!(matches!(
            DebugTarget::Attach { pid: 0 }.validate(),
            Err(DebugError::InvalidPid)
        ));
        assert!(matches!(
            DebugTarget::Dump {
                path: PathBuf::new()
            }
            .validate(),
            Err(DebugError::EmptyPath)
        ));
        assert!(matches!(
            DebugTarget::Launch {
                executable: PathBuf::from("app.exe"),
                args: vec!["bad\0arg".to_string()]
            }
            .validate(),
            Err(DebugError::ArgumentContainsNul)
        ));
    }
}
