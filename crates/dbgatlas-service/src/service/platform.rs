use super::*;

pub(super) mod job {
    #[cfg(windows)]
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    #[cfg(windows)]
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    #[cfg(windows)]
    pub struct ManagedJob {
        handle: HANDLE,
    }

    unsafe impl Send for ManagedJob {}
    unsafe impl Sync for ManagedJob {}

    #[cfg(windows)]
    impl ManagedJob {
        pub fn create_result(_name: &str) -> Result<Self, std::io::Error> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                unsafe {
                    CloseHandle(handle);
                }
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        pub fn assign_child_process(
            &self,
            process: &std::process::Child,
        ) -> Result<(), std::io::Error> {
            use std::os::windows::io::AsRawHandle;

            if self.handle.is_null() {
                return Ok(());
            }
            let ok =
                unsafe { AssignProcessToJobObject(self.handle, process.as_raw_handle() as HANDLE) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }

    #[cfg(windows)]
    impl Drop for ManagedJob {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe {
                    CloseHandle(self.handle);
                }
            }
        }
    }

    #[cfg(not(windows))]
    pub struct ManagedJob;

    #[cfg(not(windows))]
    impl ManagedJob {
        pub fn create_result(_name: &str) -> Result<Self, std::io::Error> {
            Ok(Self)
        }

        pub fn assign_child_process(
            &self,
            _process: &std::process::Child,
        ) -> Result<(), std::io::Error> {
            Ok(())
        }
    }
}

#[cfg(windows)]
pub(super) mod windows_active_user_process {
    use super::{ServiceError, WINDOWS_SERVICE_NAME};
    use std::ffi::{OsStr, OsString, c_void};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE, WAIT_FAILED};
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, TOKEN_ALL_ACCESS,
        TOKEN_ELEVATION_TYPE, TOKEN_LINKED_TOKEN, TokenElevationType, TokenElevationTypeLimited,
        TokenLinkedToken, TokenPrimary,
    };
    use windows_sys::Win32::System::Environment::{
        CreateEnvironmentBlock, DestroyEnvironmentBlock,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSGetActiveConsoleSessionId, WTSQueryUserToken,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetExitCodeProcess, INFINITE,
        PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
    };

    pub struct RawProcess {
        process_handle: HANDLE,
        thread_handle: HANDLE,
        pid: u32,
        exit_code: Option<i32>,
        waited: bool,
    }

    unsafe impl Send for RawProcess {}

    impl RawProcess {
        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn kill(&mut self) -> Result<(), io::Error> {
            if self.process_handle.is_null() || self.exit_code.is_some() {
                return Ok(());
            }
            let mut exit_code = 0;
            let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
            if ok != 0 && exit_code != STILL_ACTIVE as u32 {
                self.exit_code = Some(exit_code as i32);
                return Ok(());
            }
            let ok = unsafe { TerminateProcess(self.process_handle, 1) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn try_wait(&mut self) -> Result<Option<Option<i32>>, io::Error> {
            if let Some(exit_code) = self.exit_code {
                return Ok(Some(Some(exit_code)));
            }
            if self.process_handle.is_null() {
                return Ok(Some(None));
            }
            let mut exit_code = 0;
            let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if exit_code == STILL_ACTIVE as u32 {
                return Ok(None);
            }
            let exit_code = exit_code as i32;
            self.exit_code = Some(exit_code);
            self.waited = true;
            Ok(Some(Some(exit_code)))
        }

        pub fn wait_code(&mut self) -> Result<Option<i32>, io::Error> {
            if let Some(exit_code) = self.exit_code {
                return Ok(Some(exit_code));
            }
            if self.process_handle.is_null() {
                return Ok(None);
            }
            if !self.waited {
                let status = unsafe { WaitForSingleObject(self.process_handle, INFINITE) };
                if status == WAIT_FAILED {
                    return Err(io::Error::last_os_error());
                }
                self.waited = true;
            }
            let mut exit_code = 0;
            let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let exit_code = exit_code as i32;
            self.exit_code = Some(exit_code);
            Ok(Some(exit_code))
        }

        pub fn wait(&mut self) -> Result<(), io::Error> {
            self.wait_code().map(|_| ())
        }
    }

    impl Drop for RawProcess {
        fn drop(&mut self) {
            unsafe {
                if !self.thread_handle.is_null() {
                    CloseHandle(self.thread_handle);
                }
                if !self.process_handle.is_null() {
                    CloseHandle(self.process_handle);
                }
            }
        }
    }

    struct Handle(HANDLE);

    impl Handle {
        fn new(handle: HANDLE) -> Self {
            Self(handle)
        }

        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    struct EnvironmentBlock(*mut c_void);

    impl EnvironmentBlock {
        fn create(primary_token: HANDLE) -> Result<Self, ServiceError> {
            let mut environment = std::ptr::null_mut();
            let ok = unsafe { CreateEnvironmentBlock(&mut environment, primary_token, 0) };
            if ok == 0 {
                return Err(ServiceError::Worker(format!(
                    "CreateEnvironmentBlock for active interactive user failed: {}",
                    io::Error::last_os_error()
                )));
            }
            Ok(Self(environment))
        }

        fn raw(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for EnvironmentBlock {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DestroyEnvironmentBlock(self.0);
                }
            }
        }
    }

    pub fn spawn(worker_exe: &Path, args: &[String]) -> Result<RawProcess, ServiceError> {
        spawn_impl(worker_exe, args.iter().map(OsStr::new), "worker")
    }

    pub fn spawn_os(executable: &Path, args: &[OsString]) -> Result<RawProcess, ServiceError> {
        spawn_impl(
            executable,
            args.iter().map(|arg| arg.as_os_str()),
            "process",
        )
    }

    pub(super) fn spawn_impl<'a, I>(
        executable: &Path,
        args: I,
        process_label: &str,
    ) -> Result<RawProcess, ServiceError>
    where
        I: IntoIterator<Item = &'a OsStr>,
    {
        let session_id = unsafe { WTSGetActiveConsoleSessionId() };
        if session_id == u32::MAX {
            return Err(ServiceError::Worker(
                "no active interactive session is available for active interactive process"
                    .to_string(),
            ));
        }

        let mut impersonation_token = std::ptr::null_mut();
        let ok = unsafe { WTSQueryUserToken(session_id, &mut impersonation_token) };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "WTSQueryUserToken for active interactive session {session_id} failed: {}",
                io::Error::last_os_error()
            )));
        }
        let impersonation_token = Handle::new(impersonation_token);

        let primary_token = primary_token_for_active_session(&impersonation_token, session_id)?;
        let environment = EnvironmentBlock::create(primary_token.raw())?;

        let mut command_line = command_line(executable.as_os_str(), args);
        let mut desktop = wide_null("winsta0\\default");
        let current_directory = executable
            .parent()
            .map(|path| path.as_os_str())
            .unwrap_or_else(|| OsStr::new("."));
        let current_directory = wide_null_os(current_directory);
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        startup.lpDesktop = desktop.as_mut_ptr();
        let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessAsUserW(
                primary_token.raw(),
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_UNICODE_ENVIRONMENT,
                environment.raw(),
                current_directory.as_ptr(),
                &startup,
                &mut process_info,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "CreateProcessAsUserW failed to launch {WINDOWS_SERVICE_NAME} {process_label} in active interactive session {session_id}: {}",
                io::Error::last_os_error()
            )));
        }

        Ok(RawProcess {
            process_handle: process_info.hProcess,
            thread_handle: process_info.hThread,
            pid: process_info.dwProcessId,
            exit_code: None,
            waited: false,
        })
    }

    fn primary_token_for_active_session(
        user_token: &Handle,
        session_id: u32,
    ) -> Result<Handle, ServiceError> {
        if token_elevation_type(user_token.raw(), session_id)? == TokenElevationTypeLimited {
            let linked_token = linked_elevated_token(user_token.raw(), session_id)?;
            return duplicate_primary_token(
                linked_token.raw(),
                &format!("elevated linked token for active interactive session {session_id}"),
            );
        }
        duplicate_primary_token(
            user_token.raw(),
            &format!("active interactive session {session_id}"),
        )
    }

    pub(super) fn token_elevation_type(
        token: HANDLE,
        session_id: u32,
    ) -> Result<TOKEN_ELEVATION_TYPE, ServiceError> {
        let mut elevation_type: TOKEN_ELEVATION_TYPE = 0;
        let mut return_length = 0;
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenElevationType,
                &mut elevation_type as *mut _ as *mut c_void,
                std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
                &mut return_length,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "GetTokenInformation(TokenElevationType) for active interactive session {session_id} failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(elevation_type)
    }

    fn linked_elevated_token(token: HANDLE, session_id: u32) -> Result<Handle, ServiceError> {
        let mut linked: TOKEN_LINKED_TOKEN = unsafe { std::mem::zeroed() };
        let mut return_length = 0;
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenLinkedToken,
                &mut linked as *mut _ as *mut c_void,
                std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32,
                &mut return_length,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "GetTokenInformation(TokenLinkedToken) for active interactive session {session_id} failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Handle::new(linked.LinkedToken))
    }

    fn duplicate_primary_token(token: HANDLE, context: &str) -> Result<Handle, ServiceError> {
        let mut primary_token = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateTokenEx(
                token,
                TOKEN_ALL_ACCESS,
                std::ptr::null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut primary_token,
            )
        };
        if ok == 0 {
            return Err(ServiceError::Worker(format!(
                "DuplicateTokenEx for {context} failed: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Handle::new(primary_token))
    }

    pub(super) fn command_line<'a, I>(executable: &OsStr, args: I) -> Vec<u16>
    where
        I: IntoIterator<Item = &'a OsStr>,
    {
        let args = args.into_iter();
        let (lower_bound, _) = args.size_hint();
        let mut parts = Vec::with_capacity(lower_bound + 1);
        parts.push(quote_arg(&executable.to_string_lossy()));
        parts.extend(args.map(|arg| quote_arg(&arg.to_string_lossy())));
        wide_null(&parts.join(" "))
    }

    pub(super) fn wide_null(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn wide_null_os(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub(super) fn quote_arg(arg: &str) -> String {
        if arg.is_empty() {
            return "\"\"".to_string();
        }
        let needs_quotes = arg
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'));
        if !needs_quotes {
            return arg.to_string();
        }
        let mut quoted = String::from("\"");
        let mut backslashes = 0;
        for ch in arg.chars() {
            match ch {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                _ => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes));
                    backslashes = 0;
                    quoted.push(ch);
                }
            }
        }
        quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        quoted.push('"');
        quoted
    }
}

#[cfg(windows)]
pub(super) mod suspended_process {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CreateProcessW, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW,
        TerminateProcess,
    };

    pub struct SuspendedProcess {
        process_handle: HANDLE,
        thread_handle: HANDLE,
        pid: u32,
        resumed: bool,
    }

    impl SuspendedProcess {
        pub fn create(executable: &Path, args: &[String]) -> Result<Self, io::Error> {
            let mut command_line = command_line(executable.as_os_str(), args);
            let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                CreateProcessW(
                    std::ptr::null(),
                    command_line.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    CREATE_SUSPENDED,
                    std::ptr::null(),
                    std::ptr::null(),
                    &startup,
                    &mut process_info,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                process_handle: process_info.hProcess,
                thread_handle: process_info.hThread,
                pid: process_info.dwProcessId,
                resumed: false,
            })
        }

        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn resume(mut self) -> Result<(), io::Error> {
            let previous = unsafe { ResumeThread(self.thread_handle) };
            if previous == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            self.resumed = true;
            Ok(())
        }
    }

    impl Drop for SuspendedProcess {
        fn drop(&mut self) {
            if !self.resumed && !self.process_handle.is_null() {
                unsafe {
                    TerminateProcess(self.process_handle, 1);
                }
            }
            unsafe {
                if !self.thread_handle.is_null() {
                    CloseHandle(self.thread_handle);
                }
                if !self.process_handle.is_null() {
                    CloseHandle(self.process_handle);
                }
            }
        }
    }

    pub(super) fn command_line(executable: &OsStr, args: &[String]) -> Vec<u16> {
        let mut parts = Vec::with_capacity(args.len() + 1);
        parts.push(quote_arg(&executable.to_string_lossy()));
        parts.extend(args.iter().map(|arg| quote_arg(arg)));
        OsStr::new(&parts.join(" "))
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn quote_arg(arg: &str) -> String {
        if arg.is_empty() {
            return "\"\"".to_string();
        }
        let needs_quotes = arg
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'));
        if !needs_quotes {
            return arg.to_string();
        }

        let mut quoted = String::from("\"");
        let mut backslashes = 0;
        for ch in arg.chars() {
            if ch == '\\' {
                backslashes += 1;
                continue;
            }
            if ch == '"' {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
            } else {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(ch);
            }
            backslashes = 0;
        }
        quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        quoted.push('"');
        quoted
    }
}
