use std::ffi::{c_char, c_void};

pub const DA_OK: i32 = 0;
pub const DA_ERR_INVALID_ARGUMENT: i32 = 1;
pub const DA_ERR_BUFFER_TOO_SMALL: i32 = 2;
pub const DA_ERR_INTERNAL: i32 = 500;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DA_Version {
    pub struct_size: u32,
    pub flags: u32,
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DA_TextView {
    pub struct_size: u32,
    pub flags: u32,
    pub data: *const c_char,
    pub len: usize,
    pub owner: *mut c_void,
}

impl Default for DA_TextView {
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

#[cfg(windows)]
#[link(name = "dbgatlas_dbgeng")]
unsafe extern "C" {
    pub fn da_abi_version(out: *mut DA_Version) -> i32;
    pub fn da_native_hello(input_utf8: *const c_char, out: *mut DA_TextView) -> i32;
    pub fn da_release_view(owner: *mut c_void);
    pub fn da_last_error(buffer: *mut c_char, buffer_len: usize, required_len: *mut usize) -> i32;
}
