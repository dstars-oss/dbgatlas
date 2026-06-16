use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecordingError {
    #[error("recording launch executable must not be empty")]
    EmptyExecutable,
    #[error("recording attach pid must not be zero")]
    InvalidAttachPid,
    #[error("recording preset list must not be empty")]
    EmptyPresets,
    #[error("recording arguments must not contain control characters")]
    ControlCharacters,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordingTarget {
    Launch {
        executable: PathBuf,
        args: Vec<String>,
    },
    Attach {
        pid: u32,
    },
}

impl RecordingTarget {
    pub fn validate(self) -> Result<Self, RecordingError> {
        match self {
            Self::Launch { executable, args } => {
                validate_executable(&executable)?;
                validate_args(&args)?;
                Ok(Self::Launch { executable, args })
            }
            Self::Attach { pid } => {
                if pid == 0 {
                    return Err(RecordingError::InvalidAttachPid);
                }
                Ok(Self::Attach { pid })
            }
        }
    }

    pub fn mode(&self) -> &'static str {
        match self {
            Self::Launch { .. } => "launch",
            Self::Attach { .. } => "attach",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingPreset {
    Process,
    Thread,
    Image,
    File,
    Registry,
    Network,
}

impl RecordingPreset {
    pub const DEFAULTS: [Self; 6] = [
        Self::Process,
        Self::Thread,
        Self::Image,
        Self::File,
        Self::Registry,
        Self::Network,
    ];

    pub fn artifact_file_name(self) -> &'static str {
        match self {
            Self::Process => "process.jsonl",
            Self::Thread => "thread.jsonl",
            Self::Image => "image.jsonl",
            Self::File => "file.jsonl",
            Self::Registry => "registry.jsonl",
            Self::Network => "network.jsonl",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Thread => "thread",
            Self::Image => "image",
            Self::File => "file",
            Self::Registry => "registry",
            Self::Network => "network",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRecording {
    pub target: RecordingTarget,
    #[serde(default = "default_presets")]
    pub presets: Vec<RecordingPreset>,
}

impl StartRecording {
    pub fn validate(self) -> Result<Self, RecordingError> {
        if self.presets.is_empty() {
            return Err(RecordingError::EmptyPresets);
        }
        Ok(Self {
            target: self.target.validate()?,
            presets: self.presets,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Canceled,
    Failed,
    Killed,
}

impl RecordingState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::Canceled | Self::Failed | Self::Killed
        )
    }
}

pub fn default_presets() -> Vec<RecordingPreset> {
    RecordingPreset::DEFAULTS.to_vec()
}

fn validate_executable(path: &Path) -> Result<(), RecordingError> {
    if path.as_os_str().is_empty() {
        return Err(RecordingError::EmptyExecutable);
    }
    let text = path.as_os_str().to_string_lossy();
    if has_control_characters(&text) {
        return Err(RecordingError::ControlCharacters);
    }
    Ok(())
}

fn validate_args(args: &[String]) -> Result<(), RecordingError> {
    if args.iter().any(|arg| has_control_characters(arg)) {
        return Err(RecordingError::ControlCharacters);
    }
    Ok(())
}

fn has_control_characters(value: &str) -> bool {
    value
        .chars()
        .any(|ch| matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}') || ch.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_launch_target() {
        let request = StartRecording {
            target: RecordingTarget::Launch {
                executable: PathBuf::from(r"C:\Windows\System32\notepad.exe"),
                args: vec!["sample.txt".to_string()],
            },
            presets: default_presets(),
        };

        assert!(request.validate().is_ok());
    }

    #[test]
    fn rejects_zero_attach_pid() {
        let request = StartRecording {
            target: RecordingTarget::Attach { pid: 0 },
            presets: default_presets(),
        };

        assert!(matches!(
            request.validate(),
            Err(RecordingError::InvalidAttachPid)
        ));
    }

    #[test]
    fn rejects_empty_presets() {
        let request = StartRecording {
            target: RecordingTarget::Attach { pid: 42 },
            presets: Vec::new(),
        };

        assert!(matches!(
            request.validate(),
            Err(RecordingError::EmptyPresets)
        ));
    }
}
