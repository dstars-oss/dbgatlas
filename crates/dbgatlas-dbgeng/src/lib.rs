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
    #[error("attach pid must be greater than zero")]
    InvalidPid,
    #[error("dump path must be valid UTF-8")]
    NonUtf8Path,
    #[error("debug command must not be empty")]
    EmptyCommand,
    #[error("symbol path must not be empty")]
    EmptySymbolPath,
    #[error("memory read length must be greater than zero")]
    EmptyMemoryRead,
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
        session_from_status(handle, status)
    }

    #[cfg(not(windows))]
    pub fn open_dump(path: impl AsRef<Path>) -> Result<Self, DbgEngError> {
        path_to_cstring(path.as_ref())?;
        Err(DbgEngError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    pub fn attach(pid: u32) -> Result<Self, DbgEngError> {
        if pid == 0 {
            return Err(DbgEngError::InvalidPid);
        }
        let mut handle = std::ptr::null_mut();
        let status =
            unsafe { dbgatlas_dbgeng_sys::da_dbgeng_session_attach_process(pid, &mut handle) };
        session_from_status(handle, status)
    }

    #[cfg(not(windows))]
    pub fn attach(pid: u32) -> Result<Self, DbgEngError> {
        if pid == 0 {
            return Err(DbgEngError::InvalidPid);
        }
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

    #[cfg(windows)]
    pub fn add_symbols(&self, symbol_path: &str, reload: bool) -> Result<String, DbgEngError> {
        let symbol_path = symbol_path_to_cstring(symbol_path)?;
        let mut view = dbgatlas_dbgeng_sys::DA_DbgEngTextView::default();
        let status = unsafe {
            dbgatlas_dbgeng_sys::da_dbgeng_session_add_symbols(
                self.handle,
                symbol_path.as_ptr(),
                i32::from(reload),
                &mut view,
            )
        };
        status_to_result(status)?;
        text_view_to_string(view)
    }

    #[cfg(not(windows))]
    pub fn add_symbols(&self, symbol_path: &str, _reload: bool) -> Result<String, DbgEngError> {
        symbol_path_to_cstring(symbol_path)?;
        Err(DbgEngError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    pub fn read_memory(&self, address: u64, length: u32) -> Result<Vec<u8>, DbgEngError> {
        if length == 0 {
            return Err(DbgEngError::EmptyMemoryRead);
        }
        let mut view = dbgatlas_dbgeng_sys::DA_DbgEngTextView::default();
        let status = unsafe {
            dbgatlas_dbgeng_sys::da_dbgeng_session_read_virtual(
                self.handle,
                address,
                length,
                &mut view,
            )
        };
        status_to_result(status)?;
        text_view_to_bytes(view)
    }

    #[cfg(not(windows))]
    pub fn read_memory(&self, _address: u64, length: u32) -> Result<Vec<u8>, DbgEngError> {
        if length == 0 {
            return Err(DbgEngError::EmptyMemoryRead);
        }
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
fn session_from_status(
    handle: *mut dbgatlas_dbgeng_sys::DA_DbgEngSessionHandle,
    status: i32,
) -> Result<DbgEngSession, DbgEngError> {
    status_to_result(status)?;
    if handle.is_null() {
        return Err(DbgEngError::NullSessionHandle);
    }
    Ok(DbgEngSession { handle })
}

#[cfg(windows)]
fn text_view_to_string(
    view: dbgatlas_dbgeng_sys::DA_DbgEngTextView,
) -> Result<String, DbgEngError> {
    Ok(String::from_utf8_lossy(&text_view_to_bytes(view)?).into_owned())
}

#[cfg(windows)]
fn text_view_to_bytes(
    view: dbgatlas_dbgeng_sys::DA_DbgEngTextView,
) -> Result<Vec<u8>, DbgEngError> {
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
    let bytes = bytes.to_vec();
    release_view(view.owner);
    Ok(bytes)
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

fn symbol_path_to_cstring(symbol_path: &str) -> Result<std::ffi::CString, DbgEngError> {
    if symbol_path.trim().is_empty() {
        return Err(DbgEngError::EmptySymbolPath);
    }
    std::ffi::CString::new(symbol_path).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn dbgeng_session_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static DBGENG_SESSION_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        DBGENG_SESSION_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
    fn rejects_empty_symbol_path() {
        assert!(matches!(
            symbol_path_to_cstring("  "),
            Err(DbgEngError::EmptySymbolPath)
        ));
    }

    #[test]
    fn rejects_zero_attach_pid() {
        assert!(matches!(
            DbgEngSession::attach(0),
            Err(DbgEngError::InvalidPid)
        ));
    }

    #[test]
    fn rejects_zero_memory_read_length() {
        #[cfg(windows)]
        let session = DbgEngSession {
            handle: std::ptr::NonNull::dangling().as_ptr(),
        };
        #[cfg(not(windows))]
        let session = DbgEngSession { _private: () };

        assert!(matches!(
            session.read_memory(0x1000, 0),
            Err(DbgEngError::EmptyMemoryRead)
        ));
        std::mem::forget(session);
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
    fn opening_missing_dump_reports_native_failure() {
        let _guard = dbgeng_session_test_guard();
        let error = DbgEngSession::open_dump("dbgatlas-missing-sample.dmp").unwrap_err();
        assert!(matches!(error, DbgEngError::Native { .. }));
    }

    #[cfg(windows)]
    #[test]
    fn opens_minidump_executes_command_adds_symbols_and_reads_memory() {
        let _guard = dbgeng_session_test_guard();
        let marker = Box::new(0x1122_3344_5566_7788u64);
        let marker_address = (&*marker as *const u64) as u64;
        let temp = tempfile::tempdir().unwrap();
        let dump_path = temp.path().join("self.dmp");
        write_current_process_minidump(&dump_path);

        let session = DbgEngSession::open_dump(&dump_path).unwrap();
        let output = session.execute(".echo dbgatlas-probe").unwrap();
        assert!(output.contains("dbgatlas-probe"));

        let symbol_output = session.add_symbols(r"cache*C:\symbols", false).unwrap();
        assert!(symbol_output.contains("symbol path appended"));

        let bytes = session
            .read_memory(marker_address, size_of::<u64>() as u32)
            .unwrap();
        assert_eq!(bytes, (*marker).to_le_bytes());
    }

    #[cfg(windows)]
    #[test]
    fn attach_close_does_not_terminate_target_process() {
        let _guard = dbgeng_session_test_guard();
        let mut child = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
            .spawn()
            .unwrap();

        let session = DbgEngSession::attach(child.id()).unwrap();
        let output = session.execute(".echo attached").unwrap();
        assert!(output.contains("attached"));
        drop(session);

        assert!(child.try_wait().unwrap().is_none());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(windows)]
    fn write_current_process_minidump(path: &std::path::Path) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Diagnostics::Debug::{
            MiniDumpWithFullMemory, MiniDumpWriteDump,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};

        let file = std::fs::File::create(path).unwrap();
        let ok = unsafe {
            MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                file.as_raw_handle(),
                MiniDumpWithFullMemory,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_ne!(ok, 0, "MiniDumpWriteDump failed");
    }
}
