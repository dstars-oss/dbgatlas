use std::ffi::c_char;

pub const DA_ETW_OK: i32 = 0;
pub const DA_ETW_ERR_INVALID_ARGUMENT: i32 = 1;
pub const DA_ETW_ERR_NOT_IMPLEMENTED: i32 = 2;
pub const DA_ETW_ERR_BUFFER_TOO_SMALL: i32 = 3;
pub const DA_ETW_ERR_INTERNAL: i32 = 500;

pub const DA_ETW_CAP_REALTIME_CONSUME: u32 = 1 << 0;
pub const DA_ETW_CAP_FILE_TRACE: u32 = 1 << 1;
pub const DA_ETW_CAP_PROCESS_TREE_FILTER: u32 = 1 << 2;
pub const DA_ETW_CAP_EVENT_STACK_TRACE: u32 = 1 << 3;

pub const DA_ETW_PRESET_PROCESS: u32 = 1 << 0;
pub const DA_ETW_PRESET_THREAD: u32 = 1 << 1;
pub const DA_ETW_PRESET_IMAGE: u32 = 1 << 2;
pub const DA_ETW_PRESET_FILE: u32 = 1 << 3;
pub const DA_ETW_PRESET_REGISTRY: u32 = 1 << 4;
pub const DA_ETW_PRESET_NETWORK: u32 = 1 << 5;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_EtwVersion {
    pub struct_size: u32,
    pub flags: u32,
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_EtwAdapterInfo {
    pub struct_size: u32,
    pub flags: u32,
    pub capability_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_EtwEventExtractionResult {
    pub struct_size: u32,
    pub events_written: u32,
    pub files_written: u32,
    pub skipped_events: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_EtwStackTraceStatus {
    pub struct_size: u32,
    pub flags: u32,
    pub requested: u32,
    pub enabled: u32,
    pub provider_stack_enabled: u32,
    pub provider_stack_warning_count: u32,
    pub kernel_stack_enabled: u32,
    pub kernel_stack_warning_count: u32,
}

#[repr(C)]
pub struct DA_EtwSessionHandle {
    _private: [u8; 0],
}

#[cfg(windows)]
#[link(name = "dbgatlas_etw")]
unsafe extern "C" {
    pub fn da_etw_abi_version(out: *mut DA_EtwVersion) -> i32;
    pub fn da_etw_adapter_info(out: *mut DA_EtwAdapterInfo) -> i32;
    pub fn da_etw_last_error(
        buffer: *mut c_char,
        buffer_len: usize,
        required_len: *mut usize,
    ) -> i32;
    pub fn da_etw_write_minimal_file_trace(
        session_name_utf8: *const c_char,
        trace_path_utf8: *const c_char,
        preset_flags: u32,
    ) -> i32;
    pub fn da_etw_session_start_file_trace(
        session_name_utf8: *const c_char,
        trace_path_utf8: *const c_char,
        preset_flags: u32,
        out_handle: *mut *mut DA_EtwSessionHandle,
    ) -> i32;
    pub fn da_etw_session_start_realtime_consumer(
        handle: *mut DA_EtwSessionHandle,
        events_dir_utf8: *const c_char,
        preset_flags: u32,
        has_root_pid: u32,
        root_pid: u32,
    ) -> i32;
    pub fn da_etw_session_stack_trace_status(
        handle: *mut DA_EtwSessionHandle,
        out: *mut DA_EtwStackTraceStatus,
    ) -> i32;
    pub fn da_etw_session_stop(handle: *mut DA_EtwSessionHandle) -> i32;
    pub fn da_etw_extract_file_events(
        trace_path_utf8: *const c_char,
        events_dir_utf8: *const c_char,
        preset_flags: u32,
        has_root_pid: u32,
        root_pid: u32,
        out: *mut DA_EtwEventExtractionResult,
    ) -> i32;
    pub fn da_etw_filter_trace_file(
        input_trace_path_utf8: *const c_char,
        output_trace_path_utf8: *const c_char,
        preset_flags: u32,
        has_root_pid: u32,
        root_pid: u32,
        out: *mut DA_EtwEventExtractionResult,
    ) -> i32;
}
