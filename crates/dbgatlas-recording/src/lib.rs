use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
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
    #[error("TTD timeout_ms must be greater than zero")]
    InvalidTtdTimeout,
    #[error("TTD max_file_mb must be greater than zero")]
    InvalidTtdMaxFile,
    #[error("TTD ring max_file_mb must be at most 32768")]
    TtdRingFileTooLarge,
    #[error("TTD max_file_mb must be at most 1048576")]
    TtdFileTooLarge,
    #[error("TTD launch executable must not be empty")]
    EmptyTtdExecutable,
    #[error("TTD attach pid must not be zero")]
    InvalidTtdAttachPid,
    #[error("TTD monitor program must not be empty")]
    EmptyTtdMonitorProgram,
    #[error("TTD monitor program must be a file name or an absolute path")]
    InvalidTtdMonitorProgram,
    #[error("TTD monitor cmd_line_filter must not be empty")]
    EmptyTtdCommandLineFilter,
    #[error("TTD modules list is too large")]
    TtdModulesTooLarge,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordTtd {
    pub target: TtdTarget,
    pub timeout_ms: u64,
    #[serde(default)]
    pub options: TtdRecordingOptions,
}

impl RecordTtd {
    pub fn validate(self) -> Result<Self, RecordingError> {
        if self.timeout_ms == 0 {
            return Err(RecordingError::InvalidTtdTimeout);
        }
        let target = self.target.validate()?;
        self.options.validate(&target)?;
        Ok(Self {
            target,
            timeout_ms: self.timeout_ms,
            options: self.options,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TtdTarget {
    Launch {
        executable: PathBuf,
        #[serde(default)]
        args: Vec<String>,
    },
    Attach {
        pid: u32,
    },
    Monitor {
        program: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cmd_line_filter: Option<String>,
    },
}

impl TtdTarget {
    pub fn validate(self) -> Result<Self, RecordingError> {
        match self {
            Self::Launch { executable, args } => {
                validate_ttd_path(&executable, RecordingError::EmptyTtdExecutable)?;
                validate_args(&args)?;
                Ok(Self::Launch { executable, args })
            }
            Self::Attach { pid } => {
                if pid == 0 {
                    return Err(RecordingError::InvalidTtdAttachPid);
                }
                Ok(Self::Attach { pid })
            }
            Self::Monitor {
                program,
                cmd_line_filter,
            } => {
                validate_ttd_monitor_program(&program)?;
                if let Some(filter) = &cmd_line_filter {
                    if filter.trim().is_empty() {
                        return Err(RecordingError::EmptyTtdCommandLineFilter);
                    }
                    if has_control_characters(filter) {
                        return Err(RecordingError::ControlCharacters);
                    }
                }
                Ok(Self::Monitor {
                    program,
                    cmd_line_filter,
                })
            }
        }
    }

    pub fn mode(&self) -> &'static str {
        match self {
            Self::Launch { .. } => "launch",
            Self::Attach { .. } => "attach",
            Self::Monitor { .. } => "monitor",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TtdRecordingOptions {
    pub children: bool,
    pub no_ui: bool,
    pub accept_eula: bool,
    pub ring: bool,
    pub max_file_mb: u32,
    pub modules: Vec<String>,
    pub record_mode: TtdRecordMode,
    pub replay_cpu_support: TtdReplayCpuSupport,
}

impl Default for TtdRecordingOptions {
    fn default() -> Self {
        Self {
            children: false,
            no_ui: true,
            accept_eula: false,
            ring: false,
            max_file_mb: 2048,
            modules: Vec::new(),
            record_mode: TtdRecordMode::Automatic,
            replay_cpu_support: TtdReplayCpuSupport::Default,
        }
    }
}

impl TtdRecordingOptions {
    pub fn validate(&self, target: &TtdTarget) -> Result<(), RecordingError> {
        if self.max_file_mb == 0 {
            return Err(RecordingError::InvalidTtdMaxFile);
        }
        if self.ring && self.max_file_mb > 32768 {
            return Err(RecordingError::TtdRingFileTooLarge);
        }
        if !self.ring && self.max_file_mb > 1_048_576 {
            return Err(RecordingError::TtdFileTooLarge);
        }
        if !matches!(target, TtdTarget::Monitor { .. }) && self.modules.len() > 64 {
            return Err(RecordingError::TtdModulesTooLarge);
        }
        for module in &self.modules {
            if module.trim().is_empty() || has_control_characters(module) {
                return Err(RecordingError::ControlCharacters);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtdRecordMode {
    Automatic,
    Manual,
}

impl TtdRecordMode {
    pub fn as_ttd_arg(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Manual => "Manual",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtdReplayCpuSupport {
    Default,
    MostConservative,
    MostAggressive,
    IntelAvxRequired,
    IntelAvx2Required,
}

impl TtdReplayCpuSupport {
    pub fn as_ttd_arg(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::MostConservative => "MostConservative",
            Self::MostAggressive => "MostAggressive",
            Self::IntelAvxRequired => "IntelAvxRequired",
            Self::IntelAvx2Required => "IntelAvx2Required",
        }
    }
}

pub fn build_ttd_args(
    target: &TtdTarget,
    options: &TtdRecordingOptions,
    traces_dir: &Path,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-out"),
        traces_dir.as_os_str().to_os_string(),
    ];
    if options.no_ui {
        args.push(OsString::from("-noUI"));
    }
    if options.accept_eula {
        args.push(OsString::from("-accepteula"));
    }
    if options.children {
        args.push(OsString::from("-children"));
    }
    if options.ring {
        args.push(OsString::from("-ring"));
    }
    args.push(OsString::from("-maxFile"));
    args.push(OsString::from(options.max_file_mb.to_string()));
    for module in &options.modules {
        args.push(OsString::from("-module"));
        args.push(OsString::from(module));
    }
    if options.record_mode != TtdRecordMode::Automatic {
        args.push(OsString::from("-recordmode"));
        args.push(OsString::from(options.record_mode.as_ttd_arg()));
    }
    if options.replay_cpu_support != TtdReplayCpuSupport::Default {
        args.push(OsString::from("-replayCpuSupport"));
        args.push(OsString::from(options.replay_cpu_support.as_ttd_arg()));
    }

    match target {
        TtdTarget::Launch {
            executable,
            args: target_args,
        } => {
            args.push(OsString::from("-launch"));
            args.push(executable.as_os_str().to_os_string());
            args.extend(target_args.iter().map(OsString::from));
        }
        TtdTarget::Attach { pid } => {
            args.push(OsString::from("-attach"));
            args.push(OsString::from(pid.to_string()));
        }
        TtdTarget::Monitor {
            program,
            cmd_line_filter,
        } => {
            if let Some(filter) = cmd_line_filter {
                args.push(OsString::from("-cmdLineFilter"));
                args.push(OsString::from(filter));
            }
            args.push(OsString::from("-monitor"));
            args.push(program.as_os_str().to_os_string());
        }
    }
    args
}

pub fn ttd_stop_target(target: &TtdTarget, recorded_pid: Option<u32>) -> Option<OsString> {
    match target {
        TtdTarget::Monitor { .. } => Some(OsString::from("all")),
        TtdTarget::Attach { pid } => Some(OsString::from(recorded_pid.unwrap_or(*pid).to_string())),
        TtdTarget::Launch { executable, .. } => recorded_pid
            .map(|pid| OsString::from(pid.to_string()))
            .or_else(|| executable.file_name().map(OsString::from)),
    }
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

fn validate_ttd_path(path: &Path, empty_error: RecordingError) -> Result<(), RecordingError> {
    if path.as_os_str().is_empty() {
        return Err(empty_error);
    }
    let text = path.as_os_str().to_string_lossy();
    if has_control_characters(&text) {
        return Err(RecordingError::ControlCharacters);
    }
    Ok(())
}

fn validate_ttd_monitor_program(path: &Path) -> Result<(), RecordingError> {
    validate_ttd_path(path, RecordingError::EmptyTtdMonitorProgram)?;
    let text = path.as_os_str().to_string_lossy();
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(RecordingError::InvalidTtdMonitorProgram);
    }
    let has_separator = text.contains('\\') || text.contains('/');
    if has_separator && !path.is_absolute() {
        return Err(RecordingError::InvalidTtdMonitorProgram);
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

    #[test]
    fn validates_ttd_request_and_rejects_bad_options() {
        let request = RecordTtd {
            target: TtdTarget::Attach { pid: 42 },
            timeout_ms: 1000,
            options: TtdRecordingOptions::default(),
        };
        assert!(request.validate().is_ok());

        let request = RecordTtd {
            target: TtdTarget::Attach { pid: 0 },
            timeout_ms: 1000,
            options: TtdRecordingOptions::default(),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::InvalidTtdAttachPid)
        ));

        let request = RecordTtd {
            target: TtdTarget::Attach { pid: 42 },
            timeout_ms: 0,
            options: TtdRecordingOptions::default(),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::InvalidTtdTimeout)
        ));
    }

    #[test]
    fn builds_ttd_launch_args() {
        let target = TtdTarget::Launch {
            executable: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            args: vec!["/C".to_string(), "exit 0".to_string()],
        };
        let options = TtdRecordingOptions {
            accept_eula: true,
            children: true,
            modules: vec!["kernel32.dll".to_string()],
            record_mode: TtdRecordMode::Manual,
            replay_cpu_support: TtdReplayCpuSupport::MostConservative,
            ..Default::default()
        };

        let args = build_ttd_args(&target, &options, Path::new(r"C:\case\traces"));
        let text = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(text.contains(&"-out".to_string()));
        assert!(text.contains(&"-accepteula".to_string()));
        assert!(text.contains(&"-children".to_string()));
        assert!(text.contains(&"-module".to_string()));
        assert!(text.contains(&"kernel32.dll".to_string()));
        assert!(text.contains(&"-recordmode".to_string()));
        assert!(text.contains(&"Manual".to_string()));
        assert!(text.contains(&"-replayCpuSupport".to_string()));
        assert!(text.contains(&"MostConservative".to_string()));
        assert!(text.contains(&"-launch".to_string()));
    }

    #[test]
    fn ttd_monitor_rejects_relative_path_with_separator() {
        let request = RecordTtd {
            target: TtdTarget::Monitor {
                program: PathBuf::from(r"bin\app.exe"),
                cmd_line_filter: None,
            },
            timeout_ms: 1000,
            options: TtdRecordingOptions::default(),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::InvalidTtdMonitorProgram)
        ));
    }
}
