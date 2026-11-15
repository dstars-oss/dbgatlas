use super::*;

pub(super) struct WorkerTransport {
    pub(super) snapshot: Arc<Mutex<Option<dbgatlas_worker_protocol::WorkerSnapshot>>>,
    pub(super) file: PipeStream,
}

pub(super) struct WorkerPipeServer {
    #[cfg(windows)]
    pub(super) handle: windows_sys::Win32::Foundation::HANDLE,
}

impl WorkerTransport {
    pub(super) fn create_server(pipe_name: &str) -> Result<WorkerPipeServer, ServiceError> {
        create_worker_pipe_server(pipe_name)
    }

    pub(super) fn into_file(self) -> PipeStream {
        self.file
    }

    pub(super) fn control(
        &mut self,
        request: dbgatlas_worker_protocol::WorkerControlRequest,
    ) -> Result<dbgatlas_worker_protocol::WorkerControlResponse, ServiceError> {
        let id = next_worker_request_id();
        let line = encode_jsonl(&WorkerEnvelope::new(id.clone(), request))?;
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        let line = read_jsonl_line_with_timeout(&mut self.file, Duration::from_secs(3))?;
        let response: WorkerEnvelope<dbgatlas_worker_protocol::WorkerControlResponse> =
            decode_jsonl(&line)?;
        if response.request_id != id {
            return Err(ServiceError::Worker("control response id mismatch".into()));
        }
        Ok(response.message)
    }
}

impl WorkerPipeServer {
    pub(super) fn connect(self, timeout_ms: u64) -> Result<WorkerTransport, ServiceError> {
        connect_worker_pipe_server(self, timeout_ms)
    }
}

pub(super) fn next_worker_request_id() -> String {
    let count = WORKER_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("worker-req-{}-{count}", Timestamp::now().unix_millis)
}

pub(super) fn read_jsonl_line(reader: &mut impl Read) -> Result<String, ServiceError> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let read = reader.read(&mut byte)?;
        if read == 0 {
            return Err(ServiceError::Worker(
                "worker pipe closed before response".to_string(),
            ));
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() > MAX_WORKER_RESPONSE_LINE_BYTES {
            discard_jsonl_line_tail(reader)?;
            return Err(ServiceError::Worker(
                "worker response line is too large".to_string(),
            ));
        }
    }
    String::from_utf8(bytes).map_err(|error| ServiceError::Worker(error.to_string()))
}

#[cfg(windows)]
pub(super) fn read_jsonl_line_with_timeout(
    reader: &mut PipeStream,
    timeout: Duration,
) -> Result<String, ServiceError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let started = Instant::now();
    let mut bytes = Vec::new();
    loop {
        let mut available = 0;
        let ok = unsafe {
            PeekNamedPipe(
                reader.as_raw_handle() as _,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if available == 0 {
            if started.elapsed() >= timeout {
                return Err(ServiceError::Worker(format!(
                    "timed out waiting for worker response after {} ms",
                    timeout.as_millis()
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        let mut byte = [0u8; 1];
        let read = reader.read(&mut byte)?;
        if read == 0 {
            return Err(ServiceError::Worker(
                "worker pipe closed before response".to_string(),
            ));
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return String::from_utf8(bytes)
                .map_err(|error| ServiceError::Worker(error.to_string()));
        }
        if bytes.len() > MAX_WORKER_RESPONSE_LINE_BYTES {
            discard_jsonl_line_tail(reader)?;
            return Err(ServiceError::Worker(
                "worker response line is too large".to_string(),
            ));
        }
    }
}

#[cfg(not(windows))]
pub(super) fn read_jsonl_line_with_timeout(
    reader: &mut PipeStream,
    _timeout: Duration,
) -> Result<String, ServiceError> {
    read_jsonl_line(reader)
}

pub(super) fn discard_jsonl_line_tail(reader: &mut impl Read) -> Result<(), ServiceError> {
    let mut byte = [0u8; 1];
    loop {
        let read = reader.read(&mut byte)?;
        if read == 0 || byte[0] == b'\n' {
            return Ok(());
        }
    }
}

#[cfg(windows)]
pub(super) fn create_worker_pipe_server(pipe_name: &str) -> Result<WorkerPipeServer, ServiceError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let wide_name: Vec<u16> = std::ffi::OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut security = worker_pipe_security()?;
    let handle = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            security.attributes_ptr(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(WorkerPipeServer { handle })
}

#[cfg(windows)]
pub(super) struct WorkerPipeSecurity {
    pub(super) descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    pub(super) attributes: windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
}

#[cfg(windows)]
impl WorkerPipeSecurity {
    pub(super) fn attributes_ptr(
        &mut self,
    ) -> *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        &mut self.attributes
    }
}

#[cfg(windows)]
impl Drop for WorkerPipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(self.descriptor as _);
            }
        }
    }
}

#[cfg(windows)]
pub(super) fn worker_pipe_security() -> Result<WorkerPipeSecurity, ServiceError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    // LocalSystem service creates the server end; active interactive user workers need client access.
    let sddl = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)";
    let wide_sddl: Vec<u16> = std::ffi::OsStr::new(sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(WorkerPipeSecurity {
        descriptor,
        attributes: SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        },
    })
}

#[cfg(not(windows))]
pub(super) fn create_worker_pipe_server(
    _pipe_name: &str,
) -> Result<WorkerPipeServer, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}

#[cfg(windows)]
pub(super) fn connect_worker_pipe_server(
    server: WorkerPipeServer,
    timeout_ms: u64,
) -> Result<WorkerTransport, ServiceError> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GetLastError, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::ConnectNamedPipe;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event;
    let connected = unsafe { ConnectNamedPipe(server.handle, &mut overlapped) };
    if connected == 0 {
        let error = unsafe { GetLastError() };
        match error {
            ERROR_PIPE_CONNECTED => {}
            ERROR_IO_PENDING => {
                let wait =
                    unsafe { WaitForSingleObject(event, timeout_ms.min(u32::MAX as u64) as u32) };
                if wait == WAIT_TIMEOUT {
                    let mut transferred = 0;
                    unsafe {
                        CancelIoEx(server.handle, &overlapped);
                        GetOverlappedResult(server.handle, &overlapped, &mut transferred, 1);
                        CloseHandle(event);
                    }
                    return Err(ServiceError::Worker(format!(
                        "timed out waiting for worker pipe connection after {timeout_ms} ms"
                    )));
                }
                if wait != WAIT_OBJECT_0 {
                    let error = std::io::Error::last_os_error();
                    let mut transferred = 0;
                    unsafe {
                        CancelIoEx(server.handle, &overlapped);
                        GetOverlappedResult(server.handle, &overlapped, &mut transferred, 1);
                        CloseHandle(event);
                    }
                    return Err(error.into());
                }
                let mut transferred = 0;
                let ok =
                    unsafe { GetOverlappedResult(server.handle, &overlapped, &mut transferred, 0) };
                if ok == 0 {
                    unsafe {
                        CloseHandle(event);
                    }
                    return Err(std::io::Error::last_os_error().into());
                }
            }
            _ => {
                unsafe {
                    CloseHandle(event);
                }
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    unsafe {
        CloseHandle(event);
    }
    let handle = server.handle;
    std::mem::forget(server);
    let file = unsafe { std::fs::File::from_raw_handle(handle as RawHandle) };
    Ok(WorkerTransport {
        file: PipeStream::from_file(file),
        snapshot: Arc::new(Mutex::new(None)),
    })
}

#[cfg(not(windows))]
pub(super) fn connect_worker_pipe_server(
    _server: WorkerPipeServer,
    _timeout_ms: u64,
) -> Result<WorkerTransport, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}

#[cfg(windows)]
impl Drop for WorkerPipeServer {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};

        if self.handle != INVALID_HANDLE_VALUE && !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}
