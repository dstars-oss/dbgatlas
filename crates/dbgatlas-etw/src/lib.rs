use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

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

    #[cfg(not(windows))]
    pub fn stop(self) -> Result<(), EtwError> {
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
    Ok(EtwEventExtractionResult {
        events_written: result.events_written,
        files_written: result.files_written,
        skipped_events: result.skipped_events,
    })
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
    Ok(EtwEventExtractionResult {
        events_written: result.events_written,
        files_written: result.files_written,
        skipped_events: result.skipped_events,
    })
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
                | dbgatlas_etw_sys::DA_ETW_CAP_PROCESS_TREE_FILTER,
        );
        assert!(capabilities.realtime_consume);
        assert!(!capabilities.file_trace);
        assert!(capabilities.process_tree_filter);
        assert!(!capabilities.event_stack_trace);
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

        assert_eq!(
            flags.bits(),
            dbgatlas_etw_sys::DA_ETW_PRESET_PROCESS | dbgatlas_etw_sys::DA_ETW_PRESET_NETWORK
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
}
