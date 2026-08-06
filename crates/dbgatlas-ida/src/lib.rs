use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use thiserror::Error;

const ADAPTER_ABI_MAJOR: u32 = 0;
const ADAPTER_ABI_MINOR: u32 = 3;

#[cfg(windows)]
mod provider;

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
    #[error("failed to load native IDA adapter: {0}")]
    DynamicLoad(String),
    #[error("failed to detect IDA runtime version from {path}: {message}")]
    RuntimeVersionDetection { path: PathBuf, message: String },
    #[error("IDA runtime version {major}.{minor} is not supported")]
    UnsupportedRuntimeVersion { major: u32, minor: u32 },
    #[error("no IDA provider is available for runtime {major}.{minor}")]
    ProviderNotFound { major: u32, minor: u32 },
    #[error("IDA provider {path} is incompatible: {message}")]
    ProviderMismatch { path: PathBuf, message: String },
    #[error("IDA session worker stopped")]
    WorkerStopped,
    #[error("artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
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
pub struct IdaProviderInfo {
    pub path: PathBuf,
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
    pub ida_sdk_version: u32,
    pub ida_major: u32,
    pub ida_minor: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdaRuntimeInfo {
    pub runtime_major: u32,
    pub runtime_minor: u32,
    pub runtime_build: u32,
    pub provider_file_name: String,
    pub ida_sdk_version: u32,
    pub abi_major: u32,
    pub abi_minor: u32,
    pub abi_patch: u32,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoreFunctionResult {
    pub function: String,
    pub result: Value,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoreFunctionArtifactResult {
    pub function: String,
    pub preview: Value,
    pub full_byte_len: u64,
    pub inline_byte_limit: u64,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

pub struct IdaSession {
    commands: mpsc::Sender<Command>,
    thread: Option<thread::JoinHandle<()>>,
    runtime_info: IdaRuntimeInfo,
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
            let (ready_tx, ready_rx) = mpsc::channel::<Result<IdaRuntimeInfo, IdaError>>();
            let thread = thread::spawn(move || {
                run_native_session(install_dir, database_path, ready_tx, commands_rx)
            });
            match ready_rx.recv().map_err(|_| IdaError::WorkerStopped)? {
                Ok(runtime_info) => Ok(Self {
                    commands: commands_tx,
                    thread: Some(thread),
                    runtime_info,
                }),
                Err(error) => {
                    let _ = thread.join();
                    Err(error)
                }
            }
        }
    }

    pub fn runtime_info(&self) -> &IdaRuntimeInfo {
        &self.runtime_info
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

    pub fn core_function(
        &self,
        function: impl Into<String>,
        arguments: Value,
    ) -> Result<CoreFunctionResult, IdaError> {
        let function = function.into();
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Core {
                function,
                arguments,
                reply: reply_tx,
            })
            .map_err(|_| IdaError::WorkerStopped)?;
        reply_rx.recv().map_err(|_| IdaError::WorkerStopped)?
    }

    pub fn core_function_to_artifact(
        &self,
        function: impl Into<String>,
        arguments: Value,
        output_path: impl AsRef<Path>,
        inline_byte_limit: usize,
    ) -> Result<CoreFunctionArtifactResult, IdaError> {
        let function = function.into();
        let output_path = output_path.as_ref().to_path_buf();
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::CoreToArtifact {
                function,
                arguments,
                output_path,
                inline_byte_limit,
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
    Core {
        function: String,
        arguments: Value,
        reply: mpsc::Sender<Result<CoreFunctionResult, IdaError>>,
    },
    CoreToArtifact {
        function: String,
        arguments: Value,
        output_path: PathBuf,
        inline_byte_limit: usize,
        reply: mpsc::Sender<Result<CoreFunctionArtifactResult, IdaError>>,
    },
    Close {
        reply: mpsc::Sender<Result<(), IdaError>>,
    },
}

#[cfg(windows)]
struct AdapterApi {
    _abi_version: dbgatlas_ida_sys::DaIdaAbiVersionFn,
    _provider_info: dbgatlas_ida_sys::DaIdaProviderInfoFn,
    release_view: dbgatlas_ida_sys::DaIdaReleaseViewFn,
    last_error: dbgatlas_ida_sys::DaIdaLastErrorFn,
    session_open: dbgatlas_ida_sys::DaIdaSessionOpenFn,
    lookup_function: dbgatlas_ida_sys::DaIdaLookupFunctionFn,
    core_function: dbgatlas_ida_sys::DaIdaCoreFunctionFn,
    core_function_to_file: dbgatlas_ida_sys::DaIdaCoreFunctionToFileFn,
    session_close: dbgatlas_ida_sys::DaIdaSessionCloseFn,
}

#[cfg(windows)]
struct AdapterLibrary {
    module: windows_sys::Win32::Foundation::HMODULE,
    _dll_directory: DllDirectory,
    api: AdapterApi,
}

#[cfg(windows)]
struct DllDirectory {
    cookie: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl DllDirectory {
    fn add(path: &Path) -> Result<Self, IdaError> {
        use windows_sys::Win32::System::LibraryLoader::AddDllDirectory;

        let path_wide = wide_null(path.as_os_str());
        let cookie = unsafe { AddDllDirectory(path_wide.as_ptr()) };
        if cookie.is_null() {
            return Err(last_os_error("AddDllDirectory"));
        }
        Ok(Self { cookie })
    }
}

#[cfg(windows)]
impl Drop for DllDirectory {
    fn drop(&mut self) {
        if !self.cookie.is_null() {
            unsafe {
                windows_sys::Win32::System::LibraryLoader::RemoveDllDirectory(self.cookie);
            }
        }
    }
}

#[cfg(windows)]
impl AdapterLibrary {
    fn load(install_dir: &Path, provider: &IdaProviderInfo) -> Result<Self, IdaError> {
        use windows_sys::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_USER_DIRS, LoadLibraryExW,
            SetDefaultDllDirectories,
        };

        let search_flags = LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_USER_DIRS;
        if unsafe { SetDefaultDllDirectories(search_flags) } == 0 {
            return Err(last_os_error("SetDefaultDllDirectories"));
        }
        let dll_directory = DllDirectory::add(install_dir)?;
        let adapter_path = &provider.path;
        let adapter_wide = wide_null(adapter_path.as_os_str());
        let load_flags = LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_USER_DIRS;
        let module =
            unsafe { LoadLibraryExW(adapter_wide.as_ptr(), std::ptr::null_mut(), load_flags) };
        if module.is_null() {
            return Err(last_os_error(&format!(
                "LoadLibraryExW({})",
                adapter_path.display()
            )));
        }

        let api = unsafe {
            let result = (|| -> Result<AdapterApi, IdaError> {
                Ok(AdapterApi {
                    _abi_version: bind_symbol(module, b"da_ida_abi_version\0")?,
                    _provider_info: bind_symbol(module, b"da_ida_provider_info\0")?,
                    release_view: bind_symbol(module, b"da_ida_release_view\0")?,
                    last_error: bind_symbol(module, b"da_ida_last_error\0")?,
                    session_open: bind_symbol(module, b"da_ida_session_open\0")?,
                    lookup_function: bind_symbol(module, b"da_ida_lookup_function\0")?,
                    core_function: bind_symbol(module, b"da_ida_core_function\0")?,
                    core_function_to_file: bind_symbol(module, b"da_ida_core_function_to_file\0")?,
                    session_close: bind_symbol(module, b"da_ida_session_close\0")?,
                })
            })();
            match result {
                Ok(api) => api,
                Err(error) => {
                    windows_sys::Win32::Foundation::FreeLibrary(module);
                    return Err(error);
                }
            }
        };

        let mut version = dbgatlas_ida_sys::DA_IdaVersion {
            struct_size: std::mem::size_of::<dbgatlas_ida_sys::DA_IdaVersion>() as u32,
            ..Default::default()
        };
        let version_status = unsafe { (api._abi_version)(&mut version) };
        if version_status != dbgatlas_ida_sys::DA_IDA_OK
            || version.abi_major != ADAPTER_ABI_MAJOR
            || version.abi_minor < ADAPTER_ABI_MINOR
        {
            unsafe { windows_sys::Win32::Foundation::FreeLibrary(module) };
            return Err(IdaError::DynamicLoad(format!(
                "IDA provider ABI {}.{} is incompatible; expected at least {}.{}",
                version.abi_major, version.abi_minor, ADAPTER_ABI_MAJOR, ADAPTER_ABI_MINOR
            )));
        }

        let mut raw_provider = dbgatlas_ida_sys::DA_IdaProviderInfo {
            struct_size: std::mem::size_of::<dbgatlas_ida_sys::DA_IdaProviderInfo>() as u32,
            ..Default::default()
        };
        let provider_status = unsafe { (api._provider_info)(&mut raw_provider) };
        if provider_status != dbgatlas_ida_sys::DA_IDA_OK
            || raw_provider.ida_sdk_version != provider.ida_sdk_version
            || raw_provider.ida_major != provider.ida_major
            || raw_provider.ida_minor != provider.ida_minor
        {
            unsafe { windows_sys::Win32::Foundation::FreeLibrary(module) };
            return Err(IdaError::ProviderMismatch {
                path: adapter_path.to_path_buf(),
                message: "provider metadata changed between discovery and load".to_string(),
            });
        }

        Ok(Self {
            module,
            _dll_directory: dll_directory,
            api,
        })
    }
}

#[cfg(windows)]
impl Drop for AdapterLibrary {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::FreeLibrary(self.module);
        }
    }
}

pub fn native_version() -> Result<NativeVersion, IdaError> {
    #[cfg(not(windows))]
    {
        return Err(IdaError::UnsupportedPlatform);
    }

    #[cfg(windows)]
    {
        Ok(NativeVersion {
            abi_major: 0,
            abi_minor: ADAPTER_ABI_MINOR,
            abi_patch: 0,
            ida_major: 0,
            ida_minor: 0,
            ida_build: 0,
        })
    }
}

pub fn available_providers() -> Result<Vec<IdaProviderInfo>, IdaError> {
    #[cfg(not(windows))]
    {
        Err(IdaError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    {
        provider::discover_providers()
    }
}

pub fn detect_runtime_info(install_dir: impl AsRef<Path>) -> Result<IdaRuntimeInfo, IdaError> {
    let install_dir = validate_path(install_dir.as_ref(), "install_dir")?;
    #[cfg(not(windows))]
    {
        let _ = install_dir;
        Err(IdaError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    {
        validate_install_dir(&install_dir)?;
        let (_, info) = resolve_runtime_provider(&install_dir)?;
        Ok(info)
    }
}

fn select_provider(
    providers: &[IdaProviderInfo],
    major: u32,
    minor: u32,
) -> Result<IdaProviderInfo, IdaError> {
    if let Some(provider) = providers
        .iter()
        .find(|provider| provider.ida_major == major && provider.ida_minor == minor)
    {
        return Ok(provider.clone());
    }
    if matches!((major, minor), (9, 3) | (9, 4)) {
        Err(IdaError::ProviderNotFound { major, minor })
    } else {
        Err(IdaError::UnsupportedRuntimeVersion { major, minor })
    }
}

#[cfg(windows)]
fn validate_install_dir(install_dir: &Path) -> Result<(), IdaError> {
    if !install_dir.is_dir() {
        return Err(IdaError::DynamicLoad(format!(
            "tools.ida.install_dir does not exist or is not a directory: {}",
            install_dir.display()
        )));
    }
    for dll in ["ida.dll", "idalib.dll"] {
        let path = install_dir.join(dll);
        if !path.is_file() {
            return Err(IdaError::DynamicLoad(format!(
                "tools.ida.install_dir is missing {dll}: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn resolve_runtime_provider(
    install_dir: &Path,
) -> Result<(IdaProviderInfo, IdaRuntimeInfo), IdaError> {
    let runtime_version = provider::detect_runtime_version(&install_dir.join("idalib.dll"))?;
    let providers = provider::discover_providers()?;
    let provider = select_provider(&providers, runtime_version.major, runtime_version.minor)?;
    let runtime_info = IdaRuntimeInfo {
        runtime_major: runtime_version.major,
        runtime_minor: runtime_version.minor,
        runtime_build: runtime_version.build,
        provider_file_name: provider
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| provider.path.display().to_string()),
        ida_sdk_version: provider.ida_sdk_version,
        abi_major: provider.abi_major,
        abi_minor: provider.abi_minor,
        abi_patch: provider.abi_patch,
    };
    Ok((provider, runtime_info))
}

#[cfg(windows)]
fn run_native_session(
    install_dir: PathBuf,
    database_path: PathBuf,
    ready: mpsc::Sender<Result<IdaRuntimeInfo, IdaError>>,
    commands: mpsc::Receiver<Command>,
) {
    if let Err(error) = validate_install_dir(&install_dir) {
        let _ = ready.send(Err(error));
        return;
    }

    let (provider, runtime_info) = match resolve_runtime_provider(&install_dir) {
        Ok(resolved) => resolved,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let adapter = match AdapterLibrary::load(&install_dir, &provider) {
        Ok(adapter) => adapter,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
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
            (adapter.api.session_open)(install_dir.as_ptr(), database_path.as_ptr(), &mut handle)
        }
    };
    if open != dbgatlas_ida_sys::DA_IDA_OK {
        let _ = ready.send(Err(native_error(&adapter, open)));
        return;
    }
    let _ = ready.send(Ok(runtime_info));

    while let Ok(command) = commands.recv() {
        match command {
            Command::Lookup {
                runtime_address,
                runtime_module_base,
                ida_image_base,
                reply,
            } => {
                let _ = reply.send(native_lookup(
                    &adapter,
                    handle,
                    runtime_address,
                    runtime_module_base,
                    ida_image_base,
                ));
            }
            Command::Core {
                function,
                arguments,
                reply,
            } => {
                let _ = reply.send(native_core(&adapter, handle, function, arguments));
            }
            Command::CoreToArtifact {
                function,
                arguments,
                output_path,
                inline_byte_limit,
                reply,
            } => {
                let _ = reply.send(native_core_to_artifact(
                    &adapter,
                    handle,
                    function,
                    arguments,
                    &output_path,
                    inline_byte_limit,
                ));
            }
            Command::Close { reply } => {
                let status = unsafe { (adapter.api.session_close)(handle) };
                handle = std::ptr::null_mut();
                let result = if status == dbgatlas_ida_sys::DA_IDA_OK {
                    Ok(())
                } else {
                    Err(native_error(&adapter, status))
                };
                let _ = reply.send(result);
                break;
            }
        }
    }

    if !handle.is_null() {
        let _ = unsafe { (adapter.api.session_close)(handle) };
    }
}

#[cfg(windows)]
fn native_core(
    adapter: &AdapterLibrary,
    handle: *mut dbgatlas_ida_sys::DA_IdaSessionHandle,
    function: String,
    arguments: Value,
) -> Result<CoreFunctionResult, IdaError> {
    let function_c = std::ffi::CString::new(function.clone())
        .map_err(|_| IdaError::NulByte { field: "function" })?;
    let arguments_json = serde_json::to_string(&arguments).map_err(|error| IdaError::Native {
        status: 1,
        message: error.to_string(),
    })?;
    let arguments_c = std::ffi::CString::new(arguments_json)
        .map_err(|_| IdaError::NulByte { field: "arguments" })?;
    let mut result = dbgatlas_ida_sys::DA_IdaCoreResult {
        struct_size: std::mem::size_of::<dbgatlas_ida_sys::DA_IdaCoreResult>() as u32,
        ..Default::default()
    };
    let status = unsafe {
        (adapter.api.core_function)(
            handle,
            function_c.as_ptr(),
            arguments_c.as_ptr(),
            &mut result,
        )
    };
    if status != dbgatlas_ida_sys::DA_IDA_OK {
        return Err(native_error(adapter, status));
    }
    let result_json = take_text_view(adapter, result.result_json)?;
    let result: Value = serde_json::from_str(&result_json).map_err(|error| IdaError::Native {
        status: 1,
        message: format!("native IDA core result is not valid JSON: {error}"),
    })?;
    let warnings = result
        .get("warnings")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(CoreFunctionResult {
        function,
        result,
        warnings,
    })
}

#[cfg(windows)]
fn native_core_to_artifact(
    adapter: &AdapterLibrary,
    handle: *mut dbgatlas_ida_sys::DA_IdaSessionHandle,
    function: String,
    arguments: Value,
    output_path: &Path,
    inline_byte_limit: usize,
) -> Result<CoreFunctionArtifactResult, IdaError> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut part_name = output_path.as_os_str().to_os_string();
    part_name.push(".part");
    let part_path = PathBuf::from(part_name);
    let _ = std::fs::remove_file(&part_path);
    let function_c = std::ffi::CString::new(function.clone())
        .map_err(|_| IdaError::NulByte { field: "function" })?;
    let arguments_json = serde_json::to_string(&arguments).map_err(|error| IdaError::Native {
        status: 1,
        message: error.to_string(),
    })?;
    let arguments_c = std::ffi::CString::new(arguments_json)
        .map_err(|_| IdaError::NulByte { field: "arguments" })?;
    let output_path_c = path_to_cstring(&part_path, "output_path")?;
    let mut result = dbgatlas_ida_sys::DA_IdaArtifactResult {
        struct_size: std::mem::size_of::<dbgatlas_ida_sys::DA_IdaArtifactResult>() as u32,
        ..Default::default()
    };
    let status = unsafe {
        (adapter.api.core_function_to_file)(
            handle,
            function_c.as_ptr(),
            arguments_c.as_ptr(),
            output_path_c.as_ptr(),
            inline_byte_limit as u64,
            &mut result,
        )
    };
    if status != dbgatlas_ida_sys::DA_IDA_OK {
        let _ = std::fs::remove_file(&part_path);
        return Err(native_error(adapter, status));
    }
    let preview_json = take_text_view(adapter, result.preview_json)?;
    let preview: Value = serde_json::from_str(&preview_json).map_err(|error| IdaError::Native {
        status: 1,
        message: format!("native IDA preview is not valid JSON: {error}"),
    })?;
    if let Err(error) = std::fs::rename(&part_path, output_path) {
        let _ = std::fs::remove_file(&part_path);
        return Err(error.into());
    }
    let warnings = preview
        .get("warnings")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(CoreFunctionArtifactResult {
        function,
        preview,
        full_byte_len: result.full_byte_len,
        inline_byte_limit: result.inline_byte_limit,
        truncated: result.truncated != 0,
        warnings,
    })
}

#[cfg(windows)]
fn native_lookup(
    adapter: &AdapterLibrary,
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
        (adapter.api.lookup_function)(
            handle,
            runtime_address,
            runtime_module_base,
            ida_image_base,
            &mut result,
        )
    };
    if status != dbgatlas_ida_sys::DA_IDA_OK {
        return Err(native_error(adapter, status));
    }
    let function_name = take_text_view(adapter, result.function_name)?;
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
fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
unsafe fn bind_symbol<T>(
    module: windows_sys::Win32::Foundation::HMODULE,
    name: &'static [u8],
) -> Result<T, IdaError>
where
    T: Copy,
{
    let proc =
        unsafe { windows_sys::Win32::System::LibraryLoader::GetProcAddress(module, name.as_ptr()) };
    let Some(proc) = proc else {
        let symbol = String::from_utf8_lossy(&name[..name.len().saturating_sub(1)]);
        return Err(IdaError::DynamicLoad(format!(
            "missing native IDA adapter export `{symbol}`"
        )));
    };
    Ok(unsafe { std::mem::transmute_copy(&proc) })
}

#[cfg(windows)]
fn last_os_error(context: &str) -> IdaError {
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    IdaError::DynamicLoad(format!("{context} failed with GetLastError={code}"))
}

#[cfg(windows)]
fn path_to_cstring(path: &Path, field: &'static str) -> Result<std::ffi::CString, IdaError> {
    std::ffi::CString::new(path.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| IdaError::NulByte { field })
}

#[cfg(windows)]
fn take_text_view(
    adapter: &AdapterLibrary,
    view: dbgatlas_ida_sys::DA_IdaTextView,
) -> Result<String, IdaError> {
    if view.data.is_null() {
        return Ok(String::new());
    }
    let bytes = unsafe { std::slice::from_raw_parts(view.data.cast::<u8>(), view.len) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    if !view.owner.is_null() {
        unsafe { (adapter.api.release_view)(view.owner) };
    }
    Ok(text)
}

#[cfg(windows)]
fn native_error(adapter: &AdapterLibrary, status: i32) -> IdaError {
    let mut required = 0usize;
    let _ = unsafe { (adapter.api.last_error)(std::ptr::null_mut(), 0, &mut required) };
    if required == 0 {
        return IdaError::Native {
            status,
            message: "unknown native IDA error".to_string(),
        };
    }
    let mut buffer = vec![0u8; required];
    let last_error_status = unsafe {
        (adapter.api.last_error)(buffer.as_mut_ptr().cast(), buffer.len(), &mut required)
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
        assert_eq!(version.abi_minor, 3);
    }

    #[cfg(windows)]
    #[test]
    fn discovers_both_versioned_providers_without_ida_runtime() {
        let providers = available_providers().unwrap();
        assert!(providers.iter().any(|provider| {
            provider.ida_sdk_version == 930 && provider.ida_major == 9 && provider.ida_minor == 3
        }));
        assert!(providers.iter().any(|provider| {
            provider.ida_sdk_version == 940 && provider.ida_major == 9 && provider.ida_minor == 4
        }));
    }

    #[cfg(windows)]
    #[test]
    fn provider_discovery_keeps_directory_precedence() {
        let source = available_providers()
            .unwrap()
            .into_iter()
            .find(|provider| provider.ida_sdk_version == 930)
            .unwrap()
            .path;
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let file_name = source.file_name().unwrap();
        std::fs::copy(&source, first.join(file_name)).unwrap();
        std::fs::copy(&source, second.join(file_name)).unwrap();

        let providers = provider::discover_providers_in(&[first.clone(), second]).unwrap();
        let selected = providers
            .iter()
            .find(|provider| provider.ida_sdk_version == 930)
            .unwrap();
        assert_eq!(selected.path, first.join(file_name));
    }

    #[cfg(windows)]
    #[test]
    fn invalid_provider_candidate_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dbgatlas_ida_sdk_invalid.dll");
        std::fs::write(&path, b"not a dll").unwrap();
        assert!(matches!(
            provider::discover_providers_in(&[temp.path().to_path_buf()]),
            Err(IdaError::ProviderMismatch { .. })
        ));
    }

    #[test]
    fn selects_exact_provider_and_fails_closed() {
        let provider = IdaProviderInfo {
            path: PathBuf::from("dbgatlas_ida_sdk_930.dll"),
            abi_major: 0,
            abi_minor: 3,
            abi_patch: 0,
            ida_sdk_version: 930,
            ida_major: 9,
            ida_minor: 3,
        };
        assert_eq!(
            select_provider(std::slice::from_ref(&provider), 9, 3).unwrap(),
            provider
        );
        assert!(matches!(
            select_provider(&[], 9, 4),
            Err(IdaError::ProviderNotFound { major: 9, minor: 4 })
        ));
        assert!(matches!(
            select_provider(&[], 9, 5),
            Err(IdaError::UnsupportedRuntimeVersion { major: 9, minor: 5 })
        ));
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
