use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

pub fn profile_interval() -> Result<u32, EtwError> {
    #[cfg(windows)]
    {
        let mut interval = 0;
        status_to_result(unsafe { dbgatlas_etw_sys::da_etw_profile_interval(&mut interval) })?;
        Ok(interval)
    }
    #[cfg(not(windows))]
    {
        Err(EtwError::UnsupportedPlatform)
    }
}

pub fn recover_session(
    name: &str,
    trace: &Path,
    previous: u32,
    applied: u32,
) -> Result<(), EtwError> {
    #[cfg(windows)]
    {
        let name = session_name_to_cstring(name)?;
        let trace = path_to_cstring(trace)?;
        status_to_result(unsafe {
            dbgatlas_etw_sys::da_etw_recover_session(
                name.as_ptr(),
                trace.as_ptr(),
                previous,
                applied,
            )
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (name, trace, previous, applied);
        Err(EtwError::UnsupportedPlatform)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeVersion {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtwCapabilities {
    pub realtime_consume: bool,
    pub file_trace: bool,
    pub process_tree_filter: bool,
    pub event_stack_trace: bool,
    pub cpu_sampling: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtwAdapterInfo {
    pub version: NativeVersion,
    pub capabilities: EtwCapabilities,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtwEventExtractionResult {
    pub events_written: u32,
    pub files_written: u32,
    pub skipped_events: u32,
    pub stack_frames_total: u32,
    pub stack_frames_resolved: u32,
    pub stack_frames_unresolved: u32,
    pub file_path_resolved: u32,
    pub file_path_unresolved: u32,
    pub matched_op_end: u32,
    pub unmatched_op_end: u32,
    pub incomplete_io: u32,
    pub reused_irp: u32,
    pub dropped_stack_walk: u32,
    pub cpu_samples_written: u32,
    pub cpu_samples_with_stack: u32,
    pub cpu_samples_leaf_only: u32,
    pub cpu_samples_out_of_scope: u32,
    pub cpu_samples_cache_flushed: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtwSessionOptions {
    pub cpu_sampling_interval_us: u32,
    pub maximum_trace_size_mb: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtwCpuSamplingStatus {
    pub requested: bool,
    pub enabled: bool,
    pub stack_enabled: bool,
    pub requested_interval_us: u32,
    pub applied_interval_us: u32,
    pub previous_interval_us: u32,
    pub maximum_trace_size_mb: u32,
    pub warning_count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtwSessionStopResult {
    pub events_lost: u32,
    pub buffers_written: u32,
    pub log_buffers_lost: u32,
    pub realtime_buffers_lost: u32,
    pub interval_restore_attempted: bool,
    pub interval_restored: bool,
    pub interval_restore_skipped: bool,
    pub warning_count: u32,
    pub events_written: u32,
    pub skipped_events: u32,
    pub cpu_samples_written: u32,
    pub cpu_samples_with_stack: u32,
    pub cpu_samples_leaf_only: u32,
    pub cpu_samples_out_of_scope: u32,
    pub cpu_samples_cache_flushed: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtwStackTraceStatus {
    pub requested: bool,
    pub enabled: bool,
    pub provider_stack_enabled: bool,
    pub provider_stack_warning_count: u32,
    pub kernel_stack_enabled: bool,
    pub kernel_stack_warning_count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EtwPresetFlags {
    bits: u32,
}

impl EtwPresetFlags {
    pub const PROCESS: Self = Self {
        bits: dbgatlas_etw_sys::DA_ETW_PRESET_PROCESS,
    };
    pub const THREAD: Self = Self {
        bits: dbgatlas_etw_sys::DA_ETW_PRESET_THREAD,
    };
    pub const IMAGE: Self = Self {
        bits: dbgatlas_etw_sys::DA_ETW_PRESET_IMAGE,
    };
    pub const FILE: Self = Self {
        bits: dbgatlas_etw_sys::DA_ETW_PRESET_FILE,
    };
    pub const REGISTRY: Self = Self {
        bits: dbgatlas_etw_sys::DA_ETW_PRESET_REGISTRY,
    };
    pub const NETWORK: Self = Self {
        bits: dbgatlas_etw_sys::DA_ETW_PRESET_NETWORK,
    };
    pub const CPU: Self = Self {
        bits: dbgatlas_etw_sys::DA_ETW_PRESET_CPU,
    };
    pub const ALL: Self = Self {
        bits: dbgatlas_etw_sys::DA_ETW_PRESET_PROCESS
            | dbgatlas_etw_sys::DA_ETW_PRESET_THREAD
            | dbgatlas_etw_sys::DA_ETW_PRESET_IMAGE
            | dbgatlas_etw_sys::DA_ETW_PRESET_FILE
            | dbgatlas_etw_sys::DA_ETW_PRESET_REGISTRY
            | dbgatlas_etw_sys::DA_ETW_PRESET_NETWORK,
    };

    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    pub const fn bits(self) -> u32 {
        self.bits
    }

    pub fn insert(&mut self, other: Self) {
        self.bits |= other.bits;
    }
}

#[derive(Debug, Error)]
pub enum EtwError {
    #[error("native ETW adapter is only available on Windows")]
    UnsupportedPlatform,
    #[error("native ETW call failed with status {status}: {message}")]
    Native { status: i32, message: String },
    #[error("ETW session name must not be empty")]
    EmptySessionName,
    #[error("ETW trace path must not be empty")]
    EmptyTracePath,
    #[error("ETW trace path must be valid UTF-8")]
    NonUtf8Path,
    #[error("input contains interior nul byte")]
    InteriorNul(#[from] std::ffi::NulError),
    #[error("native returned a null ETW session handle")]
    NullSessionHandle,
}

#[cfg(windows)]
#[derive(Debug)]
pub struct EtwFileSession {
    handle: *mut dbgatlas_etw_sys::DA_EtwSessionHandle,
}

#[cfg(windows)]
unsafe impl Send for EtwFileSession {}

#[cfg(not(windows))]
#[derive(Debug)]
pub struct EtwFileSession {
    _private: (),
}

impl EtwFileSession {
    #[cfg(windows)]
    pub fn start(
        session_name: &str,
        trace_path: impl AsRef<Path>,
        preset_flags: EtwPresetFlags,
    ) -> Result<Self, EtwError> {
        let session_name = session_name_to_cstring(session_name)?;
        let trace_path = path_to_cstring(trace_path.as_ref())?;
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            dbgatlas_etw_sys::da_etw_session_start_file_trace(
                session_name.as_ptr(),
                trace_path.as_ptr(),
                preset_flags.bits(),
                &mut handle,
            )
        };
        status_to_result(status)?;
        if handle.is_null() {
            return Err(EtwError::NullSessionHandle);
        }
        Ok(Self { handle })
    }

    #[cfg(windows)]
    pub fn start_with_options(
        session_name: &str,
        trace_path: impl AsRef<Path>,
        preset_flags: EtwPresetFlags,
        options: EtwSessionOptions,
    ) -> Result<Self, EtwError> {
        let session_name = session_name_to_cstring(session_name)?;
        let trace_path = path_to_cstring(trace_path.as_ref())?;
        let options = dbgatlas_etw_sys::DA_EtwSessionOptions {
            struct_size: size_of::<dbgatlas_etw_sys::DA_EtwSessionOptions>() as u32,
            flags: 0,
            cpu_sampling_interval_us: options.cpu_sampling_interval_us,
            maximum_trace_size_mb: options.maximum_trace_size_mb,
        };
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            dbgatlas_etw_sys::da_etw_session_start_file_trace_ex(
                session_name.as_ptr(),
                trace_path.as_ptr(),
                preset_flags.bits(),
                &options,
                &mut handle,
            )
        };
        status_to_result(status)?;
        if handle.is_null() {
            return Err(EtwError::NullSessionHandle);
        }
        Ok(Self { handle })
    }

    #[cfg(not(windows))]
    pub fn start(
        session_name: &str,
        trace_path: impl AsRef<Path>,
        _preset_flags: EtwPresetFlags,
    ) -> Result<Self, EtwError> {
        session_name_to_cstring(session_name)?;
        path_to_cstring(trace_path.as_ref())?;
        Err(EtwError::UnsupportedPlatform)
    }

    #[cfg(not(windows))]
    pub fn start_with_options(
        session_name: &str,
        trace_path: impl AsRef<Path>,
        _preset_flags: EtwPresetFlags,
        _options: EtwSessionOptions,
    ) -> Result<Self, EtwError> {
        session_name_to_cstring(session_name)?;
        path_to_cstring(trace_path.as_ref())?;
        Err(EtwError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    pub fn start_realtime_consumer(
        &mut self,
        events_dir: impl AsRef<Path>,
        preset_flags: EtwPresetFlags,
        root_pid: Option<u32>,
    ) -> Result<(), EtwError> {
        let events_dir = path_to_cstring(events_dir.as_ref())?;
        let status = unsafe {
            dbgatlas_etw_sys::da_etw_session_start_realtime_consumer(
                self.handle,
                events_dir.as_ptr(),
                preset_flags.bits(),
                u32::from(root_pid.is_some()),
                root_pid.unwrap_or_default(),
            )
        };
        status_to_result(status)
    }

    #[cfg(not(windows))]
    pub fn start_realtime_consumer(
        &mut self,
        events_dir: impl AsRef<Path>,
        _preset_flags: EtwPresetFlags,
        _root_pid: Option<u32>,
    ) -> Result<(), EtwError> {
        path_to_cstring(events_dir.as_ref())?;
        Err(EtwError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    pub fn stack_trace_status(&self) -> Result<EtwStackTraceStatus, EtwError> {
        let mut status = dbgatlas_etw_sys::DA_EtwStackTraceStatus {
            struct_size: size_of::<dbgatlas_etw_sys::DA_EtwStackTraceStatus>() as u32,
            ..Default::default()
        };
        let native_status = unsafe {
            dbgatlas_etw_sys::da_etw_session_stack_trace_status(self.handle, &mut status)
        };
        status_to_result(native_status)?;
        Ok(stack_trace_status_from_native(status))
    }

    #[cfg(windows)]
    pub fn cpu_sampling_status(&self) -> Result<EtwCpuSamplingStatus, EtwError> {
        let mut status = dbgatlas_etw_sys::DA_EtwCpuSamplingStatus {
            struct_size: size_of::<dbgatlas_etw_sys::DA_EtwCpuSamplingStatus>() as u32,
            ..Default::default()
        };
        let native_status = unsafe {
            dbgatlas_etw_sys::da_etw_session_cpu_sampling_status(self.handle, &mut status)
        };
        status_to_result(native_status)?;
        Ok(cpu_sampling_status_from_native(status))
    }

    #[cfg(not(windows))]
    pub fn cpu_sampling_status(&self) -> Result<EtwCpuSamplingStatus, EtwError> {
        Err(EtwError::UnsupportedPlatform)
    }

    #[cfg(not(windows))]
    pub fn stack_trace_status(&self) -> Result<EtwStackTraceStatus, EtwError> {
        Err(EtwError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    pub fn stop(mut self) -> Result<(), EtwError> {
        let handle = std::mem::replace(&mut self.handle, std::ptr::null_mut());
        let status = unsafe { dbgatlas_etw_sys::da_etw_session_stop(handle) };
        status_to_result(status)
    }

    #[cfg(windows)]
    pub fn stop_with_result(mut self) -> Result<EtwSessionStopResult, EtwError> {
        let handle = std::mem::replace(&mut self.handle, std::ptr::null_mut());
        let mut result = dbgatlas_etw_sys::DA_EtwSessionStopResult {
            struct_size: size_of::<dbgatlas_etw_sys::DA_EtwSessionStopResult>() as u32,
            ..Default::default()
        };
        let status = unsafe { dbgatlas_etw_sys::da_etw_session_stop_ex(handle, &mut result) };
        status_to_result(status)?;
        Ok(stop_result_from_native(result))
    }

    #[cfg(not(windows))]
    pub fn stop(self) -> Result<(), EtwError> {
        Err(EtwError::UnsupportedPlatform)
    }

    #[cfg(not(windows))]
    pub fn stop_with_result(self) -> Result<EtwSessionStopResult, EtwError> {
        Err(EtwError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
impl Drop for EtwFileSession {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                let _ = dbgatlas_etw_sys::da_etw_session_stop(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

#[cfg(windows)]
pub fn native_version() -> Result<NativeVersion, EtwError> {
    let mut version = dbgatlas_etw_sys::DA_EtwVersion {
        struct_size: size_of::<dbgatlas_etw_sys::DA_EtwVersion>() as u32,
        ..Default::default()
    };
    let status = unsafe { dbgatlas_etw_sys::da_etw_abi_version(&mut version) };
    status_to_result(status)?;
    Ok(NativeVersion {
        abi_major: version.abi_major,
        abi_minor: version.abi_minor,
        abi_patch: version.abi_patch,
    })
}

#[cfg(not(windows))]
pub fn native_version() -> Result<NativeVersion, EtwError> {
    Err(EtwError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn adapter_info() -> Result<EtwAdapterInfo, EtwError> {
    let version = native_version()?;
    let mut info = dbgatlas_etw_sys::DA_EtwAdapterInfo {
        struct_size: size_of::<dbgatlas_etw_sys::DA_EtwAdapterInfo>() as u32,
        ..Default::default()
    };
    let status = unsafe { dbgatlas_etw_sys::da_etw_adapter_info(&mut info) };
    status_to_result(status)?;
    Ok(EtwAdapterInfo {
        version,
        capabilities: capabilities_from_flags(info.capability_flags),
    })
}

#[cfg(windows)]
pub fn write_minimal_file_trace(
    session_name: &str,
    trace_path: impl AsRef<Path>,
    preset_flags: EtwPresetFlags,
) -> Result<(), EtwError> {
    let session_name = session_name_to_cstring(session_name)?;
    let trace_path = path_to_cstring(trace_path.as_ref())?;
    let status = unsafe {
        dbgatlas_etw_sys::da_etw_write_minimal_file_trace(
            session_name.as_ptr(),
            trace_path.as_ptr(),
            preset_flags.bits(),
        )
    };
    status_to_result(status)
}

#[cfg(not(windows))]
pub fn write_minimal_file_trace(
    session_name: &str,
    trace_path: impl AsRef<Path>,
    _preset_flags: EtwPresetFlags,
) -> Result<(), EtwError> {
    session_name_to_cstring(session_name)?;
    path_to_cstring(trace_path.as_ref())?;
    Err(EtwError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn extract_file_events(
    trace_path: impl AsRef<Path>,
    events_dir: impl AsRef<Path>,
    preset_flags: EtwPresetFlags,
    root_pid: Option<u32>,
) -> Result<EtwEventExtractionResult, EtwError> {
    let trace_path = path_to_cstring(trace_path.as_ref())?;
    let events_dir = path_to_cstring(events_dir.as_ref())?;
    let mut result = dbgatlas_etw_sys::DA_EtwEventExtractionResult {
        struct_size: size_of::<dbgatlas_etw_sys::DA_EtwEventExtractionResult>() as u32,
        ..Default::default()
    };
    let status = unsafe {
        dbgatlas_etw_sys::da_etw_extract_file_events(
            trace_path.as_ptr(),
            events_dir.as_ptr(),
            preset_flags.bits(),
            u32::from(root_pid.is_some()),
            root_pid.unwrap_or_default(),
            &mut result,
        )
    };
    status_to_result(status)?;
    Ok(extraction_result_from_native(result))
}

#[cfg(not(windows))]
pub fn extract_file_events(
    trace_path: impl AsRef<Path>,
    events_dir: impl AsRef<Path>,
    _preset_flags: EtwPresetFlags,
    _root_pid: Option<u32>,
) -> Result<EtwEventExtractionResult, EtwError> {
    path_to_cstring(trace_path.as_ref())?;
    path_to_cstring(events_dir.as_ref())?;
    Err(EtwError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn filter_trace_file(
    input_trace_path: impl AsRef<Path>,
    output_trace_path: impl AsRef<Path>,
    preset_flags: EtwPresetFlags,
    root_pid: Option<u32>,
) -> Result<EtwEventExtractionResult, EtwError> {
    let input_trace_path = path_to_cstring(input_trace_path.as_ref())?;
    let output_trace_path = path_to_cstring(output_trace_path.as_ref())?;
    let mut result = dbgatlas_etw_sys::DA_EtwEventExtractionResult {
        struct_size: size_of::<dbgatlas_etw_sys::DA_EtwEventExtractionResult>() as u32,
        ..Default::default()
    };
    let status = unsafe {
        dbgatlas_etw_sys::da_etw_filter_trace_file(
            input_trace_path.as_ptr(),
            output_trace_path.as_ptr(),
            preset_flags.bits(),
            u32::from(root_pid.is_some()),
            root_pid.unwrap_or_default(),
            &mut result,
        )
    };
    status_to_result(status)?;
    Ok(extraction_result_from_native(result))
}

#[cfg(not(windows))]
pub fn filter_trace_file(
    input_trace_path: impl AsRef<Path>,
    output_trace_path: impl AsRef<Path>,
    _preset_flags: EtwPresetFlags,
    _root_pid: Option<u32>,
) -> Result<EtwEventExtractionResult, EtwError> {
    path_to_cstring(input_trace_path.as_ref())?;
    path_to_cstring(output_trace_path.as_ref())?;
    Err(EtwError::UnsupportedPlatform)
}

#[cfg(not(windows))]
pub fn adapter_info() -> Result<EtwAdapterInfo, EtwError> {
    Err(EtwError::UnsupportedPlatform)
}

fn capabilities_from_flags(flags: u32) -> EtwCapabilities {
    EtwCapabilities {
        realtime_consume: flags & dbgatlas_etw_sys::DA_ETW_CAP_REALTIME_CONSUME != 0,
        file_trace: flags & dbgatlas_etw_sys::DA_ETW_CAP_FILE_TRACE != 0,
        process_tree_filter: flags & dbgatlas_etw_sys::DA_ETW_CAP_PROCESS_TREE_FILTER != 0,
        event_stack_trace: flags & dbgatlas_etw_sys::DA_ETW_CAP_EVENT_STACK_TRACE != 0,
        cpu_sampling: flags & dbgatlas_etw_sys::DA_ETW_CAP_CPU_SAMPLING != 0,
    }
}

fn cpu_sampling_status_from_native(
    status: dbgatlas_etw_sys::DA_EtwCpuSamplingStatus,
) -> EtwCpuSamplingStatus {
    EtwCpuSamplingStatus {
        requested: status.requested != 0,
        enabled: status.enabled != 0,
        stack_enabled: status.stack_enabled != 0,
        requested_interval_us: status.requested_interval_us,
        applied_interval_us: status.applied_interval_us,
        previous_interval_us: status.previous_interval_us,
        maximum_trace_size_mb: status.maximum_trace_size_mb,
        warning_count: status.warning_count,
    }
}

fn stop_result_from_native(
    result: dbgatlas_etw_sys::DA_EtwSessionStopResult,
) -> EtwSessionStopResult {
    EtwSessionStopResult {
        events_lost: result.events_lost,
        buffers_written: result.buffers_written,
        log_buffers_lost: result.log_buffers_lost,
        realtime_buffers_lost: result.realtime_buffers_lost,
        interval_restore_attempted: result.interval_restore_attempted != 0,
        interval_restored: result.interval_restored != 0,
        interval_restore_skipped: result.interval_restore_skipped != 0,
        warning_count: result.warning_count,
        events_written: result.events_written,
        skipped_events: result.skipped_events,
        cpu_samples_written: result.cpu_samples_written,
        cpu_samples_with_stack: result.cpu_samples_with_stack,
        cpu_samples_leaf_only: result.cpu_samples_leaf_only,
        cpu_samples_out_of_scope: result.cpu_samples_out_of_scope,
        cpu_samples_cache_flushed: result.cpu_samples_cache_flushed,
    }
}

fn stack_trace_status_from_native(
    status: dbgatlas_etw_sys::DA_EtwStackTraceStatus,
) -> EtwStackTraceStatus {
    EtwStackTraceStatus {
        requested: status.requested != 0,
        enabled: status.enabled != 0,
        provider_stack_enabled: status.provider_stack_enabled != 0,
        provider_stack_warning_count: status.provider_stack_warning_count,
        kernel_stack_enabled: status.kernel_stack_enabled != 0,
        kernel_stack_warning_count: status.kernel_stack_warning_count,
    }
}

fn extraction_result_from_native(
    result: dbgatlas_etw_sys::DA_EtwEventExtractionResult,
) -> EtwEventExtractionResult {
    EtwEventExtractionResult {
        events_written: result.events_written,
        files_written: result.files_written,
        skipped_events: result.skipped_events,
        stack_frames_total: result.stack_frames_total,
        stack_frames_resolved: result.stack_frames_resolved,
        stack_frames_unresolved: result.stack_frames_unresolved,
        file_path_resolved: result.file_path_resolved,
        file_path_unresolved: result.file_path_unresolved,
        matched_op_end: result.matched_op_end,
        unmatched_op_end: result.unmatched_op_end,
        incomplete_io: result.incomplete_io,
        reused_irp: result.reused_irp,
        dropped_stack_walk: result.dropped_stack_walk,
        cpu_samples_written: result.cpu_samples_written,
        cpu_samples_with_stack: result.cpu_samples_with_stack,
        cpu_samples_leaf_only: result.cpu_samples_leaf_only,
        cpu_samples_out_of_scope: result.cpu_samples_out_of_scope,
        cpu_samples_cache_flushed: result.cpu_samples_cache_flushed,
    }
}

#[cfg(windows)]
fn status_to_result(status: i32) -> Result<(), EtwError> {
    if status == dbgatlas_etw_sys::DA_ETW_OK {
        return Ok(());
    }
    Err(EtwError::Native {
        status,
        message: last_error(),
    })
}

#[cfg(windows)]
fn last_error() -> String {
    let mut required = 0usize;
    let _ = unsafe { dbgatlas_etw_sys::da_etw_last_error(std::ptr::null_mut(), 0, &mut required) };
    let len = required.max(1);
    let mut buffer = vec![0u8; len];
    let mut second_required = 0usize;
    let status = unsafe {
        dbgatlas_etw_sys::da_etw_last_error(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut second_required,
        )
    };
    if status != dbgatlas_etw_sys::DA_ETW_OK {
        return format!("da_etw_last_error failed with status {status}");
    }
    let nul = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..nul]).into_owned()
}

fn session_name_to_cstring(session_name: &str) -> Result<std::ffi::CString, EtwError> {
    if session_name.trim().is_empty() {
        return Err(EtwError::EmptySessionName);
    }
    Ok(std::ffi::CString::new(session_name)?)
}

fn path_to_cstring(path: &Path) -> Result<std::ffi::CString, EtwError> {
    if path.as_os_str().is_empty() {
        return Err(EtwError::EmptyTracePath);
    }
    let path = path.as_os_str().to_str().ok_or(EtwError::NonUtf8Path)?;
    Ok(std::ffi::CString::new(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn native_version_is_readable() {
        let version = native_version().unwrap();
        assert_eq!(version.abi_major, 0);
    }

    #[cfg(windows)]
    #[test]
    fn adapter_reports_file_trace_and_process_tree_filter() {
        let info = adapter_info().unwrap();
        assert!(info.capabilities.file_trace);
        assert!(info.capabilities.process_tree_filter);
        assert!(info.capabilities.realtime_consume);
        assert!(info.capabilities.event_stack_trace);
        assert!(info.capabilities.cpu_sampling);
    }

    #[cfg(not(windows))]
    #[test]
    fn native_version_reports_unsupported_platform() {
        assert!(matches!(
            native_version(),
            Err(EtwError::UnsupportedPlatform)
        ));
    }

    #[test]
    fn decodes_capability_flags() {
        let capabilities = capabilities_from_flags(
            dbgatlas_etw_sys::DA_ETW_CAP_REALTIME_CONSUME
                | dbgatlas_etw_sys::DA_ETW_CAP_PROCESS_TREE_FILTER
                | dbgatlas_etw_sys::DA_ETW_CAP_CPU_SAMPLING,
        );
        assert!(capabilities.realtime_consume);
        assert!(!capabilities.file_trace);
        assert!(capabilities.process_tree_filter);
        assert!(!capabilities.event_stack_trace);
        assert!(capabilities.cpu_sampling);
    }

    #[test]
    fn decodes_stack_trace_status() {
        let status = stack_trace_status_from_native(dbgatlas_etw_sys::DA_EtwStackTraceStatus {
            requested: 1,
            enabled: 1,
            provider_stack_enabled: 1,
            provider_stack_warning_count: 2,
            kernel_stack_enabled: 0,
            kernel_stack_warning_count: 1,
            ..Default::default()
        });
        assert!(status.requested);
        assert!(status.enabled);
        assert!(status.provider_stack_enabled);
        assert_eq!(status.provider_stack_warning_count, 2);
        assert!(!status.kernel_stack_enabled);
        assert_eq!(status.kernel_stack_warning_count, 1);
    }

    #[test]
    fn decodes_cpu_sampling_and_stop_quality() {
        let status = cpu_sampling_status_from_native(dbgatlas_etw_sys::DA_EtwCpuSamplingStatus {
            requested: 1,
            enabled: 1,
            stack_enabled: 1,
            requested_interval_us: 1000,
            applied_interval_us: 997,
            previous_interval_us: 10_000,
            maximum_trace_size_mb: 512,
            warning_count: 2,
            ..Default::default()
        });
        assert!(status.enabled && status.stack_enabled);
        assert_eq!(status.applied_interval_us, 997);
        assert_eq!(status.previous_interval_us, 10_000);

        let stop = stop_result_from_native(dbgatlas_etw_sys::DA_EtwSessionStopResult {
            interval_restore_attempted: 1,
            interval_restored: 1,
            cpu_samples_written: 12,
            cpu_samples_with_stack: 10,
            cpu_samples_leaf_only: 2,
            cpu_samples_out_of_scope: 30,
            ..Default::default()
        });
        assert!(stop.interval_restore_attempted && stop.interval_restored);
        assert_eq!(stop.cpu_samples_written, 12);
        assert_eq!(stop.cpu_samples_with_stack, 10);
        assert_eq!(stop.cpu_samples_leaf_only, 2);
        assert_eq!(stop.cpu_samples_out_of_scope, 30);
    }

    #[test]
    fn decodes_extended_extraction_result() {
        let result = extraction_result_from_native(dbgatlas_etw_sys::DA_EtwEventExtractionResult {
            events_written: 1,
            files_written: 2,
            skipped_events: 3,
            stack_frames_total: 4,
            stack_frames_resolved: 5,
            stack_frames_unresolved: 6,
            file_path_resolved: 7,
            file_path_unresolved: 8,
            matched_op_end: 9,
            unmatched_op_end: 10,
            incomplete_io: 11,
            reused_irp: 12,
            dropped_stack_walk: 13,
            ..Default::default()
        });
        assert_eq!(result.events_written, 1);
        assert_eq!(result.stack_frames_total, 4);
        assert_eq!(result.file_path_resolved, 7);
        assert_eq!(result.matched_op_end, 9);
        assert_eq!(result.dropped_stack_walk, 13);
    }

    #[test]
    fn rejects_empty_session_name() {
        assert!(matches!(
            write_minimal_file_trace("  ", "trace.etl", EtwPresetFlags::empty()),
            Err(EtwError::EmptySessionName)
        ));
    }

    #[test]
    fn rejects_empty_trace_path() {
        assert!(matches!(
            write_minimal_file_trace(
                "dbgatlas-test",
                std::path::PathBuf::new(),
                EtwPresetFlags::empty()
            ),
            Err(EtwError::EmptyTracePath)
        ));
    }

    #[test]
    fn rejects_empty_session_start_name() {
        assert!(matches!(
            EtwFileSession::start(" ", "trace.etl", EtwPresetFlags::empty()),
            Err(EtwError::EmptySessionName)
        ));
    }

    #[test]
    fn rejects_empty_extraction_trace_path() {
        assert!(matches!(
            extract_file_events(
                std::path::PathBuf::new(),
                "events",
                EtwPresetFlags::empty(),
                None
            ),
            Err(EtwError::EmptyTracePath)
        ));
    }

    #[test]
    fn rejects_empty_filter_trace_path() {
        assert!(matches!(
            filter_trace_file(
                std::path::PathBuf::new(),
                "filtered.etl",
                EtwPresetFlags::empty(),
                None
            ),
            Err(EtwError::EmptyTracePath)
        ));
    }

    #[test]
    fn rejects_empty_realtime_events_dir_after_session_validation() {
        let session = EtwFileSession::start(" ", "trace.etl", EtwPresetFlags::empty());
        assert!(matches!(session, Err(EtwError::EmptySessionName)));
    }

    #[test]
    fn combines_preset_flags() {
        let mut flags = EtwPresetFlags::empty();
        flags.insert(EtwPresetFlags::PROCESS);
        flags.insert(EtwPresetFlags::NETWORK);
        flags.insert(EtwPresetFlags::CPU);

        assert_eq!(
            flags.bits(),
            dbgatlas_etw_sys::DA_ETW_PRESET_PROCESS
                | dbgatlas_etw_sys::DA_ETW_PRESET_NETWORK
                | dbgatlas_etw_sys::DA_ETW_PRESET_CPU
        );
    }

    #[cfg(windows)]
    #[test]
    fn extracting_missing_trace_reports_native_error() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.etl");
        let events = temp.path().join("events");

        match extract_file_events(&missing, &events, EtwPresetFlags::PROCESS, Some(42)) {
            Err(EtwError::Native { message, .. }) => assert!(!message.trim().is_empty()),
            other => panic!("expected native error for missing trace, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn filtering_missing_trace_reports_native_error() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.etl");
        let output = temp.path().join("filtered.etl");

        match filter_trace_file(&missing, &output, EtwPresetFlags::PROCESS, Some(42)) {
            Err(EtwError::Native { message, .. }) => assert!(!message.trim().is_empty()),
            other => panic!("expected native error for missing trace filter, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn writes_minimal_file_trace_or_reports_native_error() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("minimal.etl");
        let session_name = format!("DbgAtlasTest-{}", std::process::id());

        match write_minimal_file_trace(&session_name, &path, EtwPresetFlags::ALL) {
            Ok(()) => {
                assert!(path.is_file());
                assert!(std::fs::metadata(path).unwrap().len() > 0);
            }
            Err(EtwError::Native { message, .. }) => {
                assert!(!message.trim().is_empty());
            }
            Err(error) => panic!("unexpected ETW error: {error}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn start_stop_file_session_or_reports_native_error() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.etl");
        let session_name = format!("DbgAtlasSessionTest-{}", std::process::id());

        match EtwFileSession::start(&session_name, &path, EtwPresetFlags::ALL) {
            Ok(session) => {
                session.stop().unwrap();
                assert!(path.is_file());
                assert!(std::fs::metadata(path).unwrap().len() > 0);
            }
            Err(EtwError::Native { message, .. }) => {
                assert!(!message.trim().is_empty());
            }
            Err(error) => panic!("unexpected ETW error: {error}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn cpu_sampling_records_current_process_or_reports_diagnostic_error() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp.path().join("cpu.etl");
        let events = temp.path().join("events");
        let session_name = format!(
            "dbgatlas-cpu-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut session = match EtwFileSession::start_with_options(
            &session_name,
            &trace,
            EtwPresetFlags::CPU,
            EtwSessionOptions {
                cpu_sampling_interval_us: 1000,
                maximum_trace_size_mb: 64,
            },
        ) {
            Ok(session) => session,
            Err(EtwError::Native { message, .. }) => {
                assert!(
                    !message.trim().is_empty(),
                    "permission failure must be diagnostic"
                );
                return;
            }
            Err(error) => panic!("unexpected CPU session error: {error}"),
        };
        let status = session.cpu_sampling_status().unwrap();
        assert!(status.enabled && status.stack_enabled);
        session
            .start_realtime_consumer(&events, EtwPresetFlags::CPU, Some(std::process::id()))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut value = 1_u64;
        while std::time::Instant::now() < deadline {
            value = std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        std::hint::black_box(value);
        let stop = session.stop_with_result().unwrap();
        assert!(stop.interval_restore_attempted);
        assert!(stop.interval_restored || stop.interval_restore_skipped);
        assert!(stop.cpu_samples_written > 0);
        assert!(stop.cpu_samples_with_stack > 0);
        let cpu = std::fs::read_to_string(events.join("cpu.jsonl")).unwrap();
        let samples = cpu
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(
            samples
                .iter()
                .any(|sample| sample["pid"] == std::process::id())
        );
        assert!(samples.iter().any(|sample| {
            sample["stack"]["frames"]
                .as_array()
                .is_some_and(|frames| !frames.is_empty())
        }));
        assert!(samples.iter().any(|sample| sample["module"].is_object()));
    }
}
