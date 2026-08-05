use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
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
    #[error("cpu_sampling options require the `cpu` recording preset")]
    CpuOptionsWithoutPreset,
    #[error("cpu sampling interval_us must be between 125 and 1000000")]
    InvalidCpuSamplingInterval,
    #[error("cpu sampling max_trace_size_mb must be between 64 and 4096")]
    InvalidCpuTraceSize,
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
    #[error("TTD launch start_directory must not be empty")]
    EmptyTtdStartDirectory,
    #[error("TTD launch start_directory must be an absolute path")]
    InvalidTtdStartDirectory,
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
    Cpu,
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
            Self::Cpu => "cpu.jsonl",
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
            Self::Cpu => "cpu",
        }
    }
}

pub const DEFAULT_CPU_SAMPLING_INTERVAL_US: u32 = 1_000;
pub const MIN_CPU_SAMPLING_INTERVAL_US: u32 = 125;
pub const MAX_CPU_SAMPLING_INTERVAL_US: u32 = 1_000_000;
pub const DEFAULT_CPU_TRACE_SIZE_MB: u32 = 512;
pub const MIN_CPU_TRACE_SIZE_MB: u32 = 64;
pub const MAX_CPU_TRACE_SIZE_MB: u32 = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuSamplingOptions {
    #[serde(default = "default_cpu_sampling_interval_us")]
    pub interval_us: u32,
    #[serde(default = "default_cpu_trace_size_mb")]
    pub max_trace_size_mb: u32,
}

impl Default for CpuSamplingOptions {
    fn default() -> Self {
        Self {
            interval_us: DEFAULT_CPU_SAMPLING_INTERVAL_US,
            max_trace_size_mb: DEFAULT_CPU_TRACE_SIZE_MB,
        }
    }
}

impl CpuSamplingOptions {
    pub fn validate(self) -> Result<Self, RecordingError> {
        if !(MIN_CPU_SAMPLING_INTERVAL_US..=MAX_CPU_SAMPLING_INTERVAL_US)
            .contains(&self.interval_us)
        {
            return Err(RecordingError::InvalidCpuSamplingInterval);
        }
        if !(MIN_CPU_TRACE_SIZE_MB..=MAX_CPU_TRACE_SIZE_MB).contains(&self.max_trace_size_mb) {
            return Err(RecordingError::InvalidCpuTraceSize);
        }
        Ok(self)
    }
}

fn default_cpu_sampling_interval_us() -> u32 {
    DEFAULT_CPU_SAMPLING_INTERVAL_US
}

fn default_cpu_trace_size_mb() -> u32 {
    DEFAULT_CPU_TRACE_SIZE_MB
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRecording {
    pub target: RecordingTarget,
    #[serde(default = "default_presets")]
    pub presets: Vec<RecordingPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_sampling: Option<CpuSamplingOptions>,
}

impl StartRecording {
    pub fn validate(self) -> Result<Self, RecordingError> {
        if self.presets.is_empty() {
            return Err(RecordingError::EmptyPresets);
        }
        let has_cpu = self.presets.contains(&RecordingPreset::Cpu);
        if self.cpu_sampling.is_some() && !has_cpu {
            return Err(RecordingError::CpuOptionsWithoutPreset);
        }
        let cpu_sampling = if has_cpu {
            Some(self.cpu_sampling.unwrap_or_default().validate()?)
        } else {
            None
        };
        Ok(Self {
            target: self.target.validate()?,
            presets: self.presets,
            cpu_sampling,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TtdTarget {
    Launch {
        executable: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_directory: Option<PathBuf>,
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
            Self::Launch {
                executable,
                start_directory,
                args,
            } => {
                validate_ttd_path(&executable, RecordingError::EmptyTtdExecutable)?;
                if let Some(start_directory) = &start_directory {
                    validate_ttd_start_directory(start_directory)?;
                }
                validate_args(&args)?;
                Ok(Self::Launch {
                    executable,
                    start_directory,
                    args,
                })
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
            start_directory: _,
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuProfileQuality {
    pub total_samples: u64,
    pub stacked_samples: u64,
    pub leaf_only_samples: u64,
    pub resolved_leaf_samples: u64,
    pub cache_flushed_samples: u64,
    pub invalid_lines: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CpuFrameHotspot {
    pub frame: String,
    pub exclusive_samples: u64,
    pub inclusive_samples: u64,
    pub exclusive_percent: f64,
    pub inclusive_percent: f64,
    pub estimated_exclusive_cpu_ms: f64,
    pub estimated_inclusive_cpu_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CpuStackHotspot {
    pub frames: Vec<String>,
    pub samples: u64,
    pub percent: f64,
    pub estimated_cpu_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CpuProcessDistribution {
    pub pid: u32,
    pub samples: u64,
    pub percent: f64,
    pub estimated_cpu_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CpuThreadDistribution {
    pub pid: u32,
    pub tid: u32,
    pub samples: u64,
    pub percent: f64,
    pub estimated_cpu_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CpuHotspots {
    pub schema_version: u32,
    pub interval_us: u32,
    pub estimated_cpu_time_ms: f64,
    pub quality: CpuProfileQuality,
    pub frames: Vec<CpuFrameHotspot>,
    pub stacks: Vec<CpuStackHotspot>,
    pub processes: Vec<CpuProcessDistribution>,
    pub threads: Vec<CpuThreadDistribution>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CpuProfileSummary {
    pub interval_us: u32,
    pub estimated_cpu_time_ms: f64,
    pub quality: CpuProfileQuality,
    pub top_exclusive_frames: Vec<CpuFrameHotspot>,
    pub top_inclusive_frames: Vec<CpuFrameHotspot>,
    pub top_stacks: Vec<CpuStackHotspot>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CpuAggregationArtifacts {
    pub hotspots: CpuHotspots,
    pub folded_stacks: String,
}

impl CpuHotspots {
    pub fn summary(&self, limit: usize) -> CpuProfileSummary {
        let mut exclusive = self.frames.clone();
        exclusive.sort_by(|left, right| {
            right
                .exclusive_samples
                .cmp(&left.exclusive_samples)
                .then_with(|| left.frame.cmp(&right.frame))
        });
        exclusive.truncate(limit);
        let mut inclusive = self.frames.clone();
        inclusive.sort_by(|left, right| {
            right
                .inclusive_samples
                .cmp(&left.inclusive_samples)
                .then_with(|| left.frame.cmp(&right.frame))
        });
        inclusive.truncate(limit);
        CpuProfileSummary {
            interval_us: self.interval_us,
            estimated_cpu_time_ms: self.estimated_cpu_time_ms,
            quality: self.quality.clone(),
            top_exclusive_frames: exclusive,
            top_inclusive_frames: inclusive,
            top_stacks: self.stacks.iter().take(limit).cloned().collect(),
        }
    }
}

pub fn aggregate_cpu_jsonl(
    path: &Path,
    interval_us: u32,
    top_limit: usize,
) -> std::io::Result<CpuAggregationArtifacts> {
    let input = BufReader::new(File::open(path)?);
    let mut quality = CpuProfileQuality::default();
    let mut exclusive_counts = BTreeMap::<String, u64>::new();
    let mut inclusive_counts = BTreeMap::<String, u64>::new();
    let mut stack_counts = BTreeMap::<Vec<String>, u64>::new();
    let mut process_counts = BTreeMap::<u32, u64>::new();
    let mut thread_counts = BTreeMap::<(u32, u32), u64>::new();

    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            quality.invalid_lines += 1;
            continue;
        };
        if value.get("category").and_then(Value::as_str) != Some("cpu")
            || value.get("event_type").and_then(Value::as_str) != Some("sampled_profile")
        {
            continue;
        }
        let Some(pid) = value
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
        else {
            quality.invalid_lines += 1;
            continue;
        };
        let Some(tid) = value
            .get("tid")
            .and_then(Value::as_u64)
            .and_then(|tid| u32::try_from(tid).ok())
        else {
            quality.invalid_lines += 1;
            continue;
        };
        let mut leaf_to_root = value
            .pointer("/stack/frames")
            .and_then(Value::as_array)
            .map(|frames| {
                frames
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|frame| !frame.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if leaf_to_root.is_empty() {
            if let Some(frame) = value.get("frame").and_then(Value::as_str) {
                leaf_to_root.push(frame.to_string());
            } else if let Some(address) = value.get("address").and_then(Value::as_str) {
                leaf_to_root.push(address.to_string());
            }
        }
        if leaf_to_root.is_empty() {
            quality.invalid_lines += 1;
            continue;
        }

        quality.total_samples += 1;
        let stacked = value
            .get("stacked")
            .and_then(Value::as_bool)
            .unwrap_or(leaf_to_root.len() > 1);
        if stacked {
            quality.stacked_samples += 1;
        } else {
            quality.leaf_only_samples += 1;
        }
        if value.get("module").is_some_and(|module| !module.is_null()) {
            quality.resolved_leaf_samples += 1;
        }
        if value.get("cache_flushed").and_then(Value::as_bool) == Some(true) {
            quality.cache_flushed_samples += 1;
        }

        *exclusive_counts.entry(leaf_to_root[0].clone()).or_default() += 1;
        for frame in leaf_to_root.iter().cloned().collect::<BTreeSet<_>>() {
            *inclusive_counts.entry(frame).or_default() += 1;
        }
        let mut root_to_leaf = leaf_to_root;
        root_to_leaf.reverse();
        *stack_counts.entry(root_to_leaf).or_default() += 1;
        *process_counts.entry(pid).or_default() += 1;
        *thread_counts.entry((pid, tid)).or_default() += 1;
    }

    let total = quality.total_samples;
    let percent = |count: u64| {
        if total == 0 {
            0.0
        } else {
            count as f64 * 100.0 / total as f64
        }
    };
    let cpu_ms = |count: u64| count as f64 * interval_us as f64 / 1000.0;

    let mut frame_names = exclusive_counts.keys().cloned().collect::<BTreeSet<_>>();
    frame_names.extend(inclusive_counts.keys().cloned());
    let mut frames = frame_names
        .into_iter()
        .map(|frame| {
            let exclusive_samples = exclusive_counts.get(&frame).copied().unwrap_or(0);
            let inclusive_samples = inclusive_counts.get(&frame).copied().unwrap_or(0);
            CpuFrameHotspot {
                frame,
                exclusive_samples,
                inclusive_samples,
                exclusive_percent: percent(exclusive_samples),
                inclusive_percent: percent(inclusive_samples),
                estimated_exclusive_cpu_ms: cpu_ms(exclusive_samples),
                estimated_inclusive_cpu_ms: cpu_ms(inclusive_samples),
            }
        })
        .collect::<Vec<_>>();
    frames.sort_by(|left, right| {
        right
            .inclusive_samples
            .cmp(&left.inclusive_samples)
            .then_with(|| right.exclusive_samples.cmp(&left.exclusive_samples))
            .then_with(|| left.frame.cmp(&right.frame))
    });
    frames.truncate(top_limit);

    let mut stacks = stack_counts
        .iter()
        .map(|(frames, samples)| CpuStackHotspot {
            frames: frames.clone(),
            samples: *samples,
            percent: percent(*samples),
            estimated_cpu_ms: cpu_ms(*samples),
        })
        .collect::<Vec<_>>();
    stacks.sort_by(|left, right| {
        right
            .samples
            .cmp(&left.samples)
            .then_with(|| left.frames.cmp(&right.frames))
    });
    stacks.truncate(top_limit);

    let mut processes = process_counts
        .into_iter()
        .map(|(pid, samples)| CpuProcessDistribution {
            pid,
            samples,
            percent: percent(samples),
            estimated_cpu_ms: cpu_ms(samples),
        })
        .collect::<Vec<_>>();
    processes.sort_by(|left, right| {
        right
            .samples
            .cmp(&left.samples)
            .then_with(|| left.pid.cmp(&right.pid))
    });
    processes.truncate(top_limit);

    let mut threads = thread_counts
        .into_iter()
        .map(|((pid, tid), samples)| CpuThreadDistribution {
            pid,
            tid,
            samples,
            percent: percent(samples),
            estimated_cpu_ms: cpu_ms(samples),
        })
        .collect::<Vec<_>>();
    threads.sort_by(|left, right| {
        right
            .samples
            .cmp(&left.samples)
            .then_with(|| left.pid.cmp(&right.pid))
            .then_with(|| left.tid.cmp(&right.tid))
    });
    threads.truncate(top_limit);

    let mut folded_entries = stack_counts.into_iter().collect::<Vec<_>>();
    folded_entries.sort_by(|left, right| left.0.cmp(&right.0));
    let folded_stacks = folded_entries
        .into_iter()
        .map(|(frames, count)| {
            let frames = frames
                .into_iter()
                .map(|frame| frame.replace(';', ":").replace(['\r', '\n'], " "))
                .collect::<Vec<_>>()
                .join(";");
            format!("{frames} {count}\n")
        })
        .collect::<String>();

    Ok(CpuAggregationArtifacts {
        hotspots: CpuHotspots {
            schema_version: 1,
            interval_us,
            estimated_cpu_time_ms: cpu_ms(total),
            quality,
            frames,
            stacks,
            processes,
            threads,
        },
        folded_stacks,
    })
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

fn validate_ttd_start_directory(path: &Path) -> Result<(), RecordingError> {
    validate_ttd_path(path, RecordingError::EmptyTtdStartDirectory)?;
    if !path.is_absolute() {
        return Err(RecordingError::InvalidTtdStartDirectory);
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
            cpu_sampling: None,
        };

        assert!(request.validate().is_ok());
    }

    #[test]
    fn rejects_zero_attach_pid() {
        let request = StartRecording {
            target: RecordingTarget::Attach { pid: 0 },
            presets: default_presets(),
            cpu_sampling: None,
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
            cpu_sampling: None,
        };

        assert!(matches!(
            request.validate(),
            Err(RecordingError::EmptyPresets)
        ));
    }

    #[test]
    fn validates_cpu_sampling_defaults_and_bounds() {
        let request = StartRecording {
            target: RecordingTarget::Attach { pid: 42 },
            presets: vec![RecordingPreset::Cpu],
            cpu_sampling: None,
        }
        .validate()
        .unwrap();
        assert_eq!(request.cpu_sampling, Some(CpuSamplingOptions::default()));

        let request = StartRecording {
            target: RecordingTarget::Attach { pid: 42 },
            presets: default_presets(),
            cpu_sampling: Some(CpuSamplingOptions::default()),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::CpuOptionsWithoutPreset)
        ));

        let request = StartRecording {
            target: RecordingTarget::Attach { pid: 42 },
            presets: vec![RecordingPreset::Cpu],
            cpu_sampling: Some(CpuSamplingOptions {
                interval_us: 124,
                ..Default::default()
            }),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::InvalidCpuSamplingInterval)
        ));

        let request = StartRecording {
            target: RecordingTarget::Attach { pid: 42 },
            presets: vec![RecordingPreset::Cpu],
            cpu_sampling: Some(CpuSamplingOptions {
                max_trace_size_mb: 4097,
                ..Default::default()
            }),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::InvalidCpuTraceSize)
        ));
    }

    #[test]
    fn aggregates_cpu_samples_deterministically() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cpu.jsonl");
        let sample = |pid, tid, frames: &[&str], stacked, cache_flushed| {
            serde_json::to_string(&serde_json::json!({
                "category": "cpu",
                "event_type": "sampled_profile",
                "pid": pid,
                "tid": tid,
                "frame": frames[0],
                "module": { "name": "app.exe" },
                "stacked": stacked,
                "cache_flushed": cache_flushed,
                "stack": { "order": "leaf_to_root", "frames": frames },
            }))
            .unwrap()
        };
        let input = [
            sample(10, 1, &["leaf+0x1", "root+0x2"], true, false),
            sample(10, 1, &["leaf+0x1", "root+0x2"], true, false),
            sample(10, 2, &["other+0x3"], false, true),
        ]
        .join("\n");
        std::fs::write(&path, input + "\nnot-json\n").unwrap();

        let first = aggregate_cpu_jsonl(&path, 1000, 100).unwrap();
        let second = aggregate_cpu_jsonl(&path, 1000, 100).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.hotspots.quality.total_samples, 3);
        assert_eq!(first.hotspots.quality.stacked_samples, 2);
        assert_eq!(first.hotspots.quality.leaf_only_samples, 1);
        assert_eq!(first.hotspots.quality.invalid_lines, 1);
        assert_eq!(first.hotspots.estimated_cpu_time_ms, 3.0);
        assert_eq!(first.folded_stacks, "other+0x3 1\nroot+0x2;leaf+0x1 2\n");
        let summary = first.hotspots.summary(20);
        assert_eq!(summary.top_exclusive_frames[0].frame, "leaf+0x1");
        assert_eq!(summary.top_inclusive_frames[0].frame, "leaf+0x1");
        assert_eq!(summary.top_inclusive_frames[0].inclusive_samples, 2);
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
    fn validates_ttd_launch_start_directory() {
        let start_directory = std::env::current_dir().unwrap();
        let request = RecordTtd {
            target: TtdTarget::Launch {
                executable: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
                start_directory: Some(start_directory),
                args: Vec::new(),
            },
            timeout_ms: 1000,
            options: TtdRecordingOptions::default(),
        };
        assert!(request.validate().is_ok());

        let request = RecordTtd {
            target: TtdTarget::Launch {
                executable: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
                start_directory: Some(PathBuf::new()),
                args: Vec::new(),
            },
            timeout_ms: 1000,
            options: TtdRecordingOptions::default(),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::EmptyTtdStartDirectory)
        ));

        let request = RecordTtd {
            target: TtdTarget::Launch {
                executable: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
                start_directory: Some(PathBuf::from("relative")),
                args: Vec::new(),
            },
            timeout_ms: 1000,
            options: TtdRecordingOptions::default(),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::InvalidTtdStartDirectory)
        ));

        let current_dir = std::env::current_dir().unwrap();
        let request = RecordTtd {
            target: TtdTarget::Launch {
                executable: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
                start_directory: Some(PathBuf::from(format!("{}\n", current_dir.display()))),
                args: Vec::new(),
            },
            timeout_ms: 1000,
            options: TtdRecordingOptions::default(),
        };
        assert!(matches!(
            request.validate(),
            Err(RecordingError::ControlCharacters)
        ));
    }

    #[test]
    fn builds_ttd_launch_args() {
        let start_directory = std::env::current_dir().unwrap();
        let target = TtdTarget::Launch {
            executable: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            start_directory: Some(start_directory.clone()),
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
        assert!(!text.contains(&start_directory.to_string_lossy().into_owned()));
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
    fn builds_ttd_attach_args() {
        let target = TtdTarget::Attach { pid: 4242 };
        let options = TtdRecordingOptions {
            accept_eula: true,
            ..Default::default()
        };

        let args = build_ttd_args(&target, &options, Path::new(r"C:\case\traces"));
        let text = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(text.contains(&"-out".to_string()));
        assert!(text.contains(&"-accepteula".to_string()));
        assert!(text.contains(&"-attach".to_string()));
        assert!(text.contains(&"4242".to_string()));
        assert!(!text.contains(&"-launch".to_string()));
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
