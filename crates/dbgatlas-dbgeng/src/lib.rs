use serde::{Deserialize, Serialize};
use std::path::Path;
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
    #[error("native returned a null session handle")]
    NullSessionHandle,
    #[error("native returned a null text pointer")]
    NullText,
    #[error("dump path must not be empty")]
    EmptyPath,
    #[error("dump path must be valid UTF-8")]
    NonUtf8Path,
    #[error("debug command must not be empty")]
    EmptyCommand,
    #[error("input contains interior nul byte")]
    InteriorNul(#[from] std::ffi::NulError),
}

#[cfg(windows)]
#[derive(Debug)]
pub struct DbgEngSession {
    handle: *mut dbgatlas_dbgeng_sys::DA_DbgEngSessionHandle,
}

#[cfg(not(windows))]
#[derive(Debug)]
pub struct DbgEngSession {
    _private: (),
}

impl DbgEngSession {
    #[cfg(windows)]
    pub fn open_dump(path: impl AsRef<Path>) -> Result<Self, DbgEngError> {
        let path = path_to_cstring(path.as_ref())?;
        let mut handle = std::ptr::null_mut();
        let status =
            unsafe { dbgatlas_dbgeng_sys::da_dbgeng_session_open_dump(path.as_ptr(), &mut handle) };
        status_to_result(status)?;
        if handle.is_null() {
            return Err(DbgEngError::NullSessionHandle);
        }
        Ok(Self { handle })
    }

    #[cfg(not(windows))]
    pub fn open_dump(path: impl AsRef<Path>) -> Result<Self, DbgEngError> {
        path_to_cstring(path.as_ref())?;
        Err(DbgEngError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    pub fn execute(&self, command: &str) -> Result<String, DbgEngError> {
        let command = command_to_cstring(command)?;
        let mut view = dbgatlas_dbgeng_sys::DA_DbgEngTextView::default();
        let status = unsafe {
            dbgatlas_dbgeng_sys::da_dbgeng_session_execute(self.handle, command.as_ptr(), &mut view)
        };
        status_to_result(status)?;
        text_view_to_string(view)
    }

    #[cfg(not(windows))]
    pub fn execute(&self, command: &str) -> Result<String, DbgEngError> {
        command_to_cstring(command)?;
        Err(DbgEngError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
impl Drop for DbgEngSession {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                let _ = dbgatlas_dbgeng_sys::da_dbgeng_session_close(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

#[cfg(windows)]
pub fn native_version() -> Result<NativeVersion, DbgEngError> {
    let mut version = dbgatlas_dbgeng_sys::DA_DbgEngVersion {
        struct_size: size_of::<dbgatlas_dbgeng_sys::DA_DbgEngVersion>() as u32,
        ..Default::default()
    };
    let status = unsafe { dbgatlas_dbgeng_sys::da_dbgeng_abi_version(&mut version) };
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
fn status_to_result(status: i32) -> Result<(), DbgEngError> {
    if status == dbgatlas_dbgeng_sys::DA_DBGENG_OK {
        return Ok(());
    }
    Err(DbgEngError::Native {
        status,
        message: last_error(),
    })
}

#[cfg(windows)]
fn text_view_to_string(
    view: dbgatlas_dbgeng_sys::DA_DbgEngTextView,
) -> Result<String, DbgEngError> {
    use std::slice;

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

#[cfg(windows)]
fn release_view(owner: *mut std::ffi::c_void) {
    if !owner.is_null() {
        unsafe { dbgatlas_dbgeng_sys::da_dbgeng_release_view(owner) };
    }
}

#[cfg(windows)]
fn last_error() -> String {
    let mut required = 0usize;
    let _ = unsafe {
        dbgatlas_dbgeng_sys::da_dbgeng_last_error(std::ptr::null_mut(), 0, &mut required)
    };
    let len = required.max(1);
    let mut buffer = vec![0u8; len];
    let mut second_required = 0usize;
    let status = unsafe {
        dbgatlas_dbgeng_sys::da_dbgeng_last_error(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut second_required,
        )
    };
    if status != dbgatlas_dbgeng_sys::DA_DBGENG_OK {
        return format!("da_dbgeng_last_error failed with status {status}");
    }
    let nul = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..nul]).into_owned()
}

fn path_to_cstring(path: &Path) -> Result<std::ffi::CString, DbgEngError> {
    if path.as_os_str().is_empty() {
        return Err(DbgEngError::EmptyPath);
    }
    let path = path.as_os_str().to_str().ok_or(DbgEngError::NonUtf8Path)?;
    std::ffi::CString::new(path).map_err(Into::into)
}

fn command_to_cstring(command: &str) -> Result<std::ffi::CString, DbgEngError> {
    if command.trim().is_empty() {
        return Err(DbgEngError::EmptyCommand);
    }
    std::ffi::CString::new(command).map_err(Into::into)
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

    #[cfg(not(windows))]
    #[test]
    fn native_version_reports_unsupported_platform() {
        assert!(matches!(
            native_version(),
            Err(DbgEngError::UnsupportedPlatform)
        ));
    }

    #[test]
    fn rejects_empty_dump_path() {
        assert!(matches!(
            DbgEngSession::open_dump(std::path::PathBuf::new()),
            Err(DbgEngError::EmptyPath)
        ));
    }

    #[test]
    fn rejects_dump_path_with_nul() {
        assert!(matches!(
            DbgEngSession::open_dump(std::path::PathBuf::from("bad\0path")),
            Err(DbgEngError::InteriorNul(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_non_utf8_dump_path() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let path = std::path::PathBuf::from(OsString::from_wide(&[0xD800]));
        assert!(matches!(
            DbgEngSession::open_dump(path),
            Err(DbgEngError::NonUtf8Path)
        ));
    }

    #[test]
    fn rejects_empty_debug_command() {
        assert!(matches!(
            command_to_cstring("  "),
            Err(DbgEngError::EmptyCommand)
        ));
    }

    #[test]
    fn rejects_debug_command_with_nul() {
        assert!(matches!(
            command_to_cstring(".echo bad\0command"),
            Err(DbgEngError::InteriorNul(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn session_skeleton_round_trips_command_text() {
        let session = DbgEngSession::open_dump("sample.dmp").unwrap();
        let output = session.execute(".echo probe").unwrap();
        assert!(output.contains("real DbgEng execution is not wired yet"));
        assert!(output.contains(".echo probe"));
    }
}
