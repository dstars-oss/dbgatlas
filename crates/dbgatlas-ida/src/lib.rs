use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdaError {
    #[error("IDA adapter is only supported on Windows")]
    UnsupportedPlatform,
    #[error("{field} must not be empty")]
    EmptyPath { field: &'static str },
    #[error("{field} contains a NUL byte")]
    NulByte { field: &'static str },
    #[error("native IDA adapter error {status}: {message}")]
    Native { status: i32, message: String },
    #[error("IDA session worker stopped")]
    WorkerStopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeVersion {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
    pub ida_major: u32,
    pub ida_minor: u32,
    pub ida_build: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionLookup {
    pub runtime_address: u64,
    pub runtime_module_base: u64,
    pub rva: u64,
    pub ida_image_base: u64,
    pub ida_ea: u64,
    pub function_start: u64,
    pub function_end: u64,
    pub function_name: String,
    pub found: bool,
}

pub struct IdaSession {
    commands: mpsc::Sender<Command>,
    thread: Option<thread::JoinHandle<()>>,
}

impl IdaSession {
    pub fn open(
        install_dir: impl AsRef<Path>,
        database_path: impl AsRef<Path>,
    ) -> Result<Self, IdaError> {
        let install_dir = validate_path(install_dir.as_ref(), "install_dir")?;
        let database_path = validate_path(database_path.as_ref(), "database_path")?;

        #[cfg(not(windows))]
        {
            let _ = (install_dir, database_path);
            return Err(IdaError::UnsupportedPlatform);
        }

        #[cfg(windows)]
        {
            let (commands_tx, commands_rx) = mpsc::channel::<Command>();
            let (ready_tx, ready_rx) = mpsc::channel::<Result<(), IdaError>>();
            let thread = thread::spawn(move || {
                run_native_session(install_dir, database_path, ready_tx, commands_rx)
            });
            match ready_rx.recv().map_err(|_| IdaError::WorkerStopped)? {
                Ok(()) => Ok(Self {
                    commands: commands_tx,
                    thread: Some(thread),
                }),
                Err(error) => {
                    let _ = thread.join();
                    Err(error)
                }
            }
        }
    }

    pub fn lookup_function(
        &self,
        runtime_address: u64,
        runtime_module_base: u64,
        ida_image_base: u64,
    ) -> Result<FunctionLookup, IdaError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Lookup {
                runtime_address,
                runtime_module_base,
                ida_image_base,
                reply: reply_tx,
            })
            .map_err(|_| IdaError::WorkerStopped)?;
        reply_rx.recv().map_err(|_| IdaError::WorkerStopped)?
    }

    pub fn close(mut self) -> Result<(), IdaError> {
        self.close_inner()
    }

    pub fn try_close(&mut self) -> Result<(), IdaError> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), IdaError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let send_result = self.commands.send(Command::Close { reply: reply_tx });
        let reply_result = if send_result.is_ok() {
            reply_rx.recv().map_err(|_| IdaError::WorkerStopped)?
        } else {
            Ok(())
        };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        reply_result
    }
}

impl Drop for IdaSession {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}

enum Command {
    Lookup {
        runtime_address: u64,
        runtime_module_base: u64,
        ida_image_base: u64,
        reply: mpsc::Sender<Result<FunctionLookup, IdaError>>,
    },
    Close {
        reply: mpsc::Sender<Result<(), IdaError>>,
    },
}

pub fn native_version() -> Result<NativeVersion, IdaError> {
    #[cfg(not(windows))]
    {
        return Err(IdaError::UnsupportedPlatform);
    }

    #[cfg(windows)]
    {
        let mut version = dbgatlas_ida_sys::DA_IdaVersion {
            struct_size: std::mem::size_of::<dbgatlas_ida_sys::DA_IdaVersion>() as u32,
            ..Default::default()
        };
        let status = unsafe { dbgatlas_ida_sys::da_ida_abi_version(&mut version) };
        if status == dbgatlas_ida_sys::DA_IDA_OK {
            Ok(NativeVersion {
                abi_major: version.abi_major,
                abi_minor: version.abi_minor,
                abi_patch: version.abi_patch,
                ida_major: version.ida_major,
                ida_minor: version.ida_minor,
                ida_build: version.ida_build,
            })
        } else {
            Err(native_error(status))
        }
    }
}

#[cfg(windows)]
fn run_native_session(
    install_dir: PathBuf,
    database_path: PathBuf,
    ready: mpsc::Sender<Result<(), IdaError>>,
    commands: mpsc::Receiver<Command>,
) {
    let mut handle = std::ptr::null_mut();
    let open = {
        let install_dir = match path_to_cstring(&install_dir, "install_dir") {
            Ok(value) => value,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let database_path = match path_to_cstring(&database_path, "database_path") {
            Ok(value) => value,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        unsafe {
            dbgatlas_ida_sys::da_ida_session_open(
                install_dir.as_ptr(),
                database_path.as_ptr(),
                &mut handle,
            )
        }
    };
    if open != dbgatlas_ida_sys::DA_IDA_OK {
        let _ = ready.send(Err(native_error(open)));
        return;
    }
    let _ = ready.send(Ok(()));

    while let Ok(command) = commands.recv() {
        match command {
            Command::Lookup {
                runtime_address,
                runtime_module_base,
                ida_image_base,
                reply,
            } => {
                let _ = reply.send(native_lookup(
                    handle,
                    runtime_address,
                    runtime_module_base,
                    ida_image_base,
                ));
            }
            Command::Close { reply } => {
                let status = unsafe { dbgatlas_ida_sys::da_ida_session_close(handle) };
                handle = std::ptr::null_mut();
                let result = if status == dbgatlas_ida_sys::DA_IDA_OK {
                    Ok(())
                } else {
                    Err(native_error(status))
                };
                let _ = reply.send(result);
                break;
            }
        }
    }

    if !handle.is_null() {
        let _ = unsafe { dbgatlas_ida_sys::da_ida_session_close(handle) };
    }
}

#[cfg(windows)]
fn native_lookup(
    handle: *mut dbgatlas_ida_sys::DA_IdaSessionHandle,
    runtime_address: u64,
    runtime_module_base: u64,
    ida_image_base: u64,
) -> Result<FunctionLookup, IdaError> {
    let mut result = dbgatlas_ida_sys::DA_IdaFunctionLookup {
        struct_size: std::mem::size_of::<dbgatlas_ida_sys::DA_IdaFunctionLookup>() as u32,
        ..Default::default()
    };
    let status = unsafe {
        dbgatlas_ida_sys::da_ida_lookup_function(
            handle,
            runtime_address,
            runtime_module_base,
            ida_image_base,
            &mut result,
        )
    };
    if status != dbgatlas_ida_sys::DA_IDA_OK {
        return Err(native_error(status));
    }
    let function_name = take_text_view(result.function_name)?;
    Ok(FunctionLookup {
        runtime_address: result.runtime_address,
        runtime_module_base: result.runtime_module_base,
        rva: result.rva,
        ida_image_base: result.ida_image_base,
        ida_ea: result.ida_ea,
        function_start: result.function_start,
        function_end: result.function_end,
        function_name,
        found: result.found != 0,
    })
}

fn validate_path(path: &Path, field: &'static str) -> Result<PathBuf, IdaError> {
    if path.as_os_str().is_empty() {
        return Err(IdaError::EmptyPath { field });
    }
    if path.as_os_str().to_string_lossy().contains('\0') {
        return Err(IdaError::NulByte { field });
    }
    Ok(path.to_path_buf())
}

#[cfg(windows)]
fn path_to_cstring(path: &Path, field: &'static str) -> Result<std::ffi::CString, IdaError> {
    std::ffi::CString::new(path.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| IdaError::NulByte { field })
}

#[cfg(windows)]
fn take_text_view(view: dbgatlas_ida_sys::DA_IdaTextView) -> Result<String, IdaError> {
    if view.data.is_null() {
        return Ok(String::new());
    }
    let bytes = unsafe { std::slice::from_raw_parts(view.data.cast::<u8>(), view.len) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    if !view.owner.is_null() {
        unsafe { dbgatlas_ida_sys::da_ida_release_view(view.owner) };
    }
    Ok(text)
}

#[cfg(windows)]
fn native_error(status: i32) -> IdaError {
    let mut required = 0usize;
    let _ = unsafe { dbgatlas_ida_sys::da_ida_last_error(std::ptr::null_mut(), 0, &mut required) };
    if required == 0 {
        return IdaError::Native {
            status,
            message: "unknown native IDA error".to_string(),
        };
    }
    let mut buffer = vec![0u8; required];
    let last_error_status = unsafe {
        dbgatlas_ida_sys::da_ida_last_error(buffer.as_mut_ptr().cast(), buffer.len(), &mut required)
    };
    if last_error_status != dbgatlas_ida_sys::DA_IDA_OK {
        return IdaError::Native {
            status,
            message: "unknown native IDA error".to_string(),
        };
    }
    if buffer.last().copied() == Some(0) {
        buffer.pop();
    }
    IdaError::Native {
        status,
        message: String::from_utf8_lossy(&buffer).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_install_dir() {
        let error = match IdaSession::open("", "sample.idb") {
            Ok(_) => panic!("empty install dir should be rejected"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            IdaError::EmptyPath {
                field: "install_dir"
            }
        ));
    }

    #[test]
    fn computes_lookup_shape_from_values() {
        let lookup = FunctionLookup {
            runtime_address: 0x180001234,
            runtime_module_base: 0x180000000,
            rva: 0x1234,
            ida_image_base: 0x140000000,
            ida_ea: 0x140001234,
            function_start: 0x140001000,
            function_end: 0x140001500,
            function_name: "sub_140001000".to_string(),
            found: true,
        };
        assert_eq!(
            lookup.rva,
            lookup.runtime_address - lookup.runtime_module_base
        );
        assert!(lookup.found);
    }

    #[cfg(windows)]
    #[test]
    fn native_version_is_readable_without_ida_runtime() {
        let version = native_version().unwrap();
        assert_eq!(version.abi_major, 0);
    }

    #[cfg(windows)]
    #[test]
    fn missing_install_dir_reports_native_error() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("sample.bin");
        std::fs::write(&database, b"sample").unwrap();
        let error = match IdaSession::open(temp.path().join("missing-ida"), &database) {
            Ok(_) => panic!("missing install dir should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("install_dir"));
    }
}
