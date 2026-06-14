use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeVersion {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
}

#[derive(Debug, Error)]
pub enum DbgEngError {
    #[error("native adapter is only available on Windows")]
    UnsupportedPlatform,
    #[error("native call failed with status {status}: {message}")]
    Native { status: i32, message: String },
    #[error("native returned a null text pointer")]
    NullText,
    #[error("input contains interior nul byte")]
    InteriorNul(#[from] std::ffi::NulError),
}

#[cfg(windows)]
pub fn native_version() -> Result<NativeVersion, DbgEngError> {
    let mut version = dbgatlas_dbgeng_sys::DA_Version {
        struct_size: size_of::<dbgatlas_dbgeng_sys::DA_Version>() as u32,
        ..Default::default()
    };
    let status = unsafe { dbgatlas_dbgeng_sys::da_abi_version(&mut version) };
    status_to_result(status)?;
    Ok(NativeVersion {
        abi_major: version.abi_major,
        abi_minor: version.abi_minor,
        abi_patch: version.abi_patch,
    })
}

#[cfg(not(windows))]
pub fn native_version() -> Result<NativeVersion, DbgEngError> {
    Err(DbgEngError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn native_hello(input: &str) -> Result<String, DbgEngError> {
    use std::ffi::CString;
    use std::slice;

    let input = CString::new(input)?;
    let mut view = dbgatlas_dbgeng_sys::DA_TextView::default();
    let status = unsafe { dbgatlas_dbgeng_sys::da_native_hello(input.as_ptr(), &mut view) };
    status_to_result(status)?;

    if view.data.is_null() && view.len > 0 {
        release_view(view.owner);
        return Err(DbgEngError::NullText);
    }

    let bytes = if view.len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(view.data.cast::<u8>(), view.len) }
    };
    let text = String::from_utf8_lossy(bytes).into_owned();
    release_view(view.owner);
    Ok(text)
}

#[cfg(not(windows))]
pub fn native_hello(_input: &str) -> Result<String, DbgEngError> {
    Err(DbgEngError::UnsupportedPlatform)
}

#[cfg(windows)]
fn status_to_result(status: i32) -> Result<(), DbgEngError> {
    if status == dbgatlas_dbgeng_sys::DA_OK {
        return Ok(());
    }
    Err(DbgEngError::Native {
        status,
        message: last_error(),
    })
}

#[cfg(windows)]
fn release_view(owner: *mut std::ffi::c_void) {
    if !owner.is_null() {
        unsafe { dbgatlas_dbgeng_sys::da_release_view(owner) };
    }
}

#[cfg(windows)]
fn last_error() -> String {
    let mut required = 0usize;
    let _ = unsafe { dbgatlas_dbgeng_sys::da_last_error(std::ptr::null_mut(), 0, &mut required) };
    let len = required.max(1);
    let mut buffer = vec![0u8; len];
    let mut second_required = 0usize;
    let status = unsafe {
        dbgatlas_dbgeng_sys::da_last_error(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut second_required,
        )
    };
    if status != dbgatlas_dbgeng_sys::DA_OK {
        return format!("da_last_error failed with status {status}");
    }
    let nul = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..nul]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_version_is_readable() {
        let version = native_version().unwrap();
        assert_eq!(version.abi_major, 0);
    }

    #[test]
    fn native_hello_round_trips_text() {
        let text = native_hello("probe").unwrap();
        assert!(text.contains("probe"));
    }
}
