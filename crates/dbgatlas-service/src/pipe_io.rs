//! Blocking adapters over overlapped named-pipe handles. Each operation owns its
//! OVERLAPPED/event until completion, so a pending read never serializes writes.
use std::{
    fs::File,
    io::{self, Read, Write},
};

pub struct PipeStream(File);
impl PipeStream {
    pub fn from_file(file: File) -> Self {
        Self(file)
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawHandle for PipeStream {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        std::os::windows::io::AsRawHandle::as_raw_handle(&self.0)
    }
}

#[cfg(windows)]
fn transfer(file: &File, buffer: *mut u8, len: usize, write: bool) -> io::Result<usize> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, GetLastError},
        Storage::FileSystem::{ReadFile, WriteFile},
        System::{
            IO::{GetOverlappedResult, OVERLAPPED},
            Threading::CreateEventW,
        },
    };
    if len == 0 {
        return Ok(0);
    }
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    struct Event(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Event {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    let _event = Event(event);
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event;
    let mut count = 0;
    let handle = file.as_raw_handle() as _;
    let size = len.min(u32::MAX as usize) as u32;
    let ok = unsafe {
        if write {
            WriteFile(handle, buffer, size, &mut count, &mut overlapped)
        } else {
            ReadFile(handle, buffer, size, &mut count, &mut overlapped)
        }
    };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_BROKEN_PIPE && !write {
            return Ok(0);
        }
        if error != ERROR_IO_PENDING {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        if unsafe { GetOverlappedResult(handle, &overlapped, &mut count, 1) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_BROKEN_PIPE && !write {
                return Ok(0);
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
    Ok(count as usize)
}

impl Read for PipeStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            transfer(&self.0, buffer.as_mut_ptr(), buffer.len(), false)
        }
        #[cfg(not(windows))]
        {
            self.0.read(buffer)
        }
    }
}
impl Write for PipeStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            transfer(&self.0, buffer.as_ptr().cast_mut(), buffer.len(), true)
        }
        #[cfg(not(windows))]
        {
            self.0.write(buffer)
        }
    }
    // WriteFile completion is enough. FlushFileBuffers on pipes waits for the peer
    // to consume bytes and would make an independent control path depend on it.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
