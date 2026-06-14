use std::ffi::{c_char, c_void};

pub const DA_DBGENG_OK: i32 = 0;
pub const DA_DBGENG_ERR_INVALID_ARGUMENT: i32 = 1;
pub const DA_DBGENG_ERR_BUFFER_TOO_SMALL: i32 = 2;
pub const DA_DBGENG_ERR_INTERNAL: i32 = 500;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_DbgEngVersion {
    pub struct_size: u32,
    pub flags: u32,
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DA_DbgEngTextView {
    pub struct_size: u32,
    pub flags: u32,
    pub data: *const c_char,
    pub len: usize,
    pub owner: *mut c_void,
}

impl Default for DA_DbgEngTextView {
    fn default() -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            flags: 0,
            data: std::ptr::null(),
            len: 0,
            owner: std::ptr::null_mut(),
        }
    }
}

#[repr(C)]
pub struct DA_DbgEngSessionHandle {
    _private: [u8; 0],
}

#[cfg(windows)]
#[link(name = "dbgatlas_dbgeng")]
unsafe extern "C" {
    pub fn da_dbgeng_abi_version(out: *mut DA_DbgEngVersion) -> i32;
    pub fn da_dbgeng_release_view(owner: *mut c_void);
    pub fn da_dbgeng_last_error(
        buffer: *mut c_char,
        buffer_len: usize,
        required_len: *mut usize,
    ) -> i32;
    pub fn da_dbgeng_session_open_dump(
        path_utf8: *const c_char,
        out_handle: *mut *mut DA_DbgEngSessionHandle,
    ) -> i32;
    pub fn da_dbgeng_session_attach_process(
        pid: u32,
        out_handle: *mut *mut DA_DbgEngSessionHandle,
    ) -> i32;
    pub fn da_dbgeng_session_execute(
        handle: *mut DA_DbgEngSessionHandle,
        command_utf8: *const c_char,
        out: *mut DA_DbgEngTextView,
    ) -> i32;
    pub fn da_dbgeng_session_add_symbols(
        handle: *mut DA_DbgEngSessionHandle,
        symbol_path_utf8: *const c_char,
        reload: i32,
        out: *mut DA_DbgEngTextView,
    ) -> i32;
    pub fn da_dbgeng_session_read_virtual(
        handle: *mut DA_DbgEngSessionHandle,
        address: u64,
        length: u32,
        out: *mut DA_DbgEngTextView,
    ) -> i32;
    pub fn da_dbgeng_session_close(handle: *mut DA_DbgEngSessionHandle) -> i32;
}
