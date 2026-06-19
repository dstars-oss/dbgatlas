use std::ffi::{c_char, c_void};

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
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_IdaCoreResult {
    pub struct_size: u32,
    pub flags: u32,
    pub result_json: DA_IdaTextView,
}

#[repr(C)]
pub struct DA_IdaSessionHandle {
    _private: [u8; 0],
}

pub type DaIdaAbiVersionFn = unsafe extern "C" fn(*mut DA_IdaVersion) -> i32;
pub type DaIdaReleaseViewFn = unsafe extern "C" fn(*mut c_void);
pub type DaIdaLastErrorFn = unsafe extern "C" fn(*mut c_char, usize, *mut usize) -> i32;
pub type DaIdaSessionOpenFn =
    unsafe extern "C" fn(*const c_char, *const c_char, *mut *mut DA_IdaSessionHandle) -> i32;
pub type DaIdaLookupFunctionFn =
    unsafe extern "C" fn(*mut DA_IdaSessionHandle, u64, u64, u64, *mut DA_IdaFunctionLookup) -> i32;
pub type DaIdaCoreFunctionFn = unsafe extern "C" fn(
    *mut DA_IdaSessionHandle,
    *const c_char,
    *const c_char,
    *mut DA_IdaCoreResult,
) -> i32;
pub type DaIdaSessionCloseFn = unsafe extern "C" fn(*mut DA_IdaSessionHandle) -> i32;
