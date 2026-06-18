use std::ffi::c_char;

pub const DA_IDA_OK: i32 = 0;
pub const DA_IDA_ERR_INVALID_ARGUMENT: i32 = 1;
pub const DA_IDA_ERR_BUFFER_TOO_SMALL: i32 = 2;
pub const DA_IDA_ERR_NOT_FOUND: i32 = 3;
pub const DA_IDA_ERR_DYNAMIC_LOAD: i32 = 4;
pub const DA_IDA_ERR_IDA: i32 = 5;
pub const DA_IDA_ERR_INTERNAL: i32 = 500;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_IdaVersion {
    pub struct_size: u32,
    pub flags: u32,
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
    pub ida_major: u32,
    pub ida_minor: u32,
    pub ida_build: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_IdaTextView {
    pub struct_size: u32,
    pub flags: u32,
    pub data: *const c_char,
    pub len: usize,
    pub owner: *mut std::ffi::c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_IdaFunctionLookup {
    pub struct_size: u32,
    pub flags: u32,
    pub runtime_address: u64,
    pub runtime_module_base: u64,
    pub rva: u64,
    pub ida_image_base: u64,
    pub ida_ea: u64,
    pub function_start: u64,
    pub function_end: u64,
    pub found: i32,
    pub function_name: DA_IdaTextView,
}

#[repr(C)]
pub struct DA_IdaSessionHandle {
    _private: [u8; 0],
}

#[cfg(windows)]
#[link(name = "dbgatlas_ida")]
unsafe extern "C" {
    pub fn da_ida_abi_version(out: *mut DA_IdaVersion) -> i32;
    pub fn da_ida_release_view(owner: *mut std::ffi::c_void);
    pub fn da_ida_last_error(
        buffer: *mut c_char,
        buffer_len: usize,
        required_len: *mut usize,
    ) -> i32;
    pub fn da_ida_session_open(
        install_dir_utf8: *const c_char,
        database_path_utf8: *const c_char,
        out_handle: *mut *mut DA_IdaSessionHandle,
    ) -> i32;
    pub fn da_ida_lookup_function(
        handle: *mut DA_IdaSessionHandle,
        runtime_address: u64,
        runtime_module_base: u64,
        ida_image_base: u64,
        out: *mut DA_IdaFunctionLookup,
    ) -> i32;
    pub fn da_ida_session_close(handle: *mut DA_IdaSessionHandle) -> i32;
}
