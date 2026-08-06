use crate::{ADAPTER_ABI_MAJOR, ADAPTER_ABI_MINOR, IdaError, IdaProviderInfo};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{FreeLibrary, GetLastError};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    LoadLibraryExW,
};

const VS_FIXEDFILEINFO_SIGNATURE: u32 = 0xFEEF_04BD;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

pub(crate) fn detect_runtime_version(idalib_path: &Path) -> Result<RuntimeVersion, IdaError> {
    let path_wide = wide_null(idalib_path.as_os_str());
    let mut ignored_handle = 0u32;
    let size = unsafe { GetFileVersionInfoSizeW(path_wide.as_ptr(), &mut ignored_handle) };
    if size == 0 {
        return Err(version_error(
            idalib_path,
            format!(
                "GetFileVersionInfoSizeW failed with GetLastError={}",
                unsafe { GetLastError() }
            ),
        ));
    }

    let mut data = vec![0u8; size as usize];
    if unsafe {
        GetFileVersionInfoW(
            path_wide.as_ptr(),
            0,
            size,
            data.as_mut_ptr().cast::<c_void>(),
        )
    } == 0
    {
        return Err(version_error(
            idalib_path,
            format!("GetFileVersionInfoW failed with GetLastError={}", unsafe {
                GetLastError()
            }),
        ));
    }

    let root = wide_null(std::ffi::OsStr::new("\\"));
    let mut value = std::ptr::null_mut::<c_void>();
    let mut value_len = 0u32;
    if unsafe {
        VerQueryValueW(
            data.as_ptr().cast::<c_void>(),
            root.as_ptr(),
            &mut value,
            &mut value_len,
        )
    } == 0
        || value.is_null()
        || value_len < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
    {
        return Err(version_error(
            idalib_path,
            "VERSIONINFO does not contain VS_FIXEDFILEINFO".to_string(),
        ));
    }

    let fixed = unsafe { &*value.cast::<VS_FIXEDFILEINFO>() };
    runtime_version_from_fixed(idalib_path, fixed)
}

fn runtime_version_from_fixed(
    path: &Path,
    fixed: &VS_FIXEDFILEINFO,
) -> Result<RuntimeVersion, IdaError> {
    if fixed.dwSignature != VS_FIXEDFILEINFO_SIGNATURE {
        return Err(version_error(
            path,
            format!(
                "invalid VS_FIXEDFILEINFO signature 0x{:08x}",
                fixed.dwSignature
            ),
        ));
    }

    Ok(RuntimeVersion {
        major: fixed.dwFileVersionMS >> 16,
        minor: fixed.dwFileVersionMS & 0xffff,
        build: fixed.dwFileVersionLS >> 16,
    })
}

pub(crate) fn discover_providers() -> Result<Vec<IdaProviderInfo>, IdaError> {
    discover_providers_in(&provider_search_dirs()?)
}

pub(crate) fn discover_providers_in(
    directories: &[PathBuf],
) -> Result<Vec<IdaProviderInfo>, IdaError> {
    let mut providers = BTreeMap::<(u32, u32), IdaProviderInfo>::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        let mut candidates = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| is_provider_file(path))
            .collect::<Vec<_>>();
        candidates.sort();
        for candidate in candidates {
            let info = inspect_provider(&candidate)?;
            providers
                .entry((info.ida_major, info.ida_minor))
                .or_insert(info);
        }
    }
    Ok(providers.into_values().collect())
}

fn provider_search_dirs() -> Result<Vec<PathBuf>, IdaError> {
    let exe = std::env::current_exe()
        .map_err(|error| IdaError::DynamicLoad(format!("failed to locate current exe: {error}")))?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| IdaError::DynamicLoad("current exe has no parent directory".to_string()))?;
    let mut directories = vec![exe_dir.to_path_buf(), exe_dir.join("deps")];
    if let Some(parent) = exe_dir.parent() {
        directories.push(parent.to_path_buf());
    }
    directories.dedup();
    Ok(directories)
}

fn is_provider_file(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                let name = name.to_ascii_lowercase();
                name.starts_with("dbgatlas_ida_sdk_") && name.ends_with(".dll")
            })
            .unwrap_or(false)
}

fn inspect_provider(path: &Path) -> Result<IdaProviderInfo, IdaError> {
    let path_wide = wide_null(path.as_os_str());
    let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
    let module = unsafe { LoadLibraryExW(path_wide.as_ptr(), std::ptr::null_mut(), flags) };
    if module.is_null() {
        return Err(IdaError::ProviderMismatch {
            path: path.to_path_buf(),
            message: format!("LoadLibraryExW failed with GetLastError={}", unsafe {
                GetLastError()
            }),
        });
    }

    let result = (|| {
        let proc = unsafe { GetProcAddress(module, c"da_ida_provider_info".as_ptr().cast()) }
            .ok_or_else(|| IdaError::ProviderMismatch {
                path: path.to_path_buf(),
                message: "missing export `da_ida_provider_info`".to_string(),
            })?;
        let provider_info: dbgatlas_ida_sys::DaIdaProviderInfoFn =
            unsafe { std::mem::transmute(proc) };
        let mut raw = dbgatlas_ida_sys::DA_IdaProviderInfo {
            struct_size: std::mem::size_of::<dbgatlas_ida_sys::DA_IdaProviderInfo>() as u32,
            ..Default::default()
        };
        let status = unsafe { provider_info(&mut raw) };
        if status != dbgatlas_ida_sys::DA_IDA_OK {
            return Err(IdaError::ProviderMismatch {
                path: path.to_path_buf(),
                message: format!("da_ida_provider_info failed with status {status}"),
            });
        }
        validate_provider_info(path, raw)
    })();
    unsafe { FreeLibrary(module) };
    result
}

fn validate_provider_info(
    path: &Path,
    raw: dbgatlas_ida_sys::DA_IdaProviderInfo,
) -> Result<IdaProviderInfo, IdaError> {
    if raw.abi_major != ADAPTER_ABI_MAJOR || raw.abi_minor < ADAPTER_ABI_MINOR {
        return Err(IdaError::ProviderMismatch {
            path: path.to_path_buf(),
            message: format!(
                "ABI {}.{} is incompatible; expected {}.{} or newer compatible minor",
                raw.abi_major, raw.abi_minor, ADAPTER_ABI_MAJOR, ADAPTER_ABI_MINOR
            ),
        });
    }
    let expected_sdk = raw
        .ida_major
        .checked_mul(100)
        .and_then(|value| value.checked_add(raw.ida_minor.saturating_mul(10)));
    if expected_sdk != Some(raw.ida_sdk_version) {
        return Err(IdaError::ProviderMismatch {
            path: path.to_path_buf(),
            message: format!(
                "SDK {} does not match declared runtime {}.{}",
                raw.ida_sdk_version, raw.ida_major, raw.ida_minor
            ),
        });
    }
    Ok(IdaProviderInfo {
        path: path.to_path_buf(),
        abi_major: raw.abi_major,
        abi_minor: raw.abi_minor,
        abi_patch: raw.abi_patch,
        ida_sdk_version: raw.ida_sdk_version,
        ida_major: raw.ida_major,
        ida_minor: raw.ida_minor,
    })
}

fn version_error(path: &Path, message: String) -> IdaError {
    IdaError::RuntimeVersionDetection {
        path: path.to_path_buf(),
        message,
    }
}

fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixed_file_version() {
        let mut fixed = unsafe { std::mem::zeroed::<VS_FIXEDFILEINFO>() };
        fixed.dwSignature = VS_FIXEDFILEINFO_SIGNATURE;
        fixed.dwFileVersionMS = (9 << 16) | 4;
        fixed.dwFileVersionLS = (26 << 16) | 327;

        assert_eq!(
            runtime_version_from_fixed(Path::new("idalib.dll"), &fixed).unwrap(),
            RuntimeVersion {
                major: 9,
                minor: 4,
                build: 26,
            }
        );
    }

    #[test]
    fn rejects_invalid_fixed_file_version_signature() {
        let fixed = unsafe { std::mem::zeroed::<VS_FIXEDFILEINFO>() };
        assert!(matches!(
            runtime_version_from_fixed(Path::new("idalib.dll"), &fixed),
            Err(IdaError::RuntimeVersionDetection { .. })
        ));
    }
}
