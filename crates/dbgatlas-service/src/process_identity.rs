//! Instance-checked process ownership shared by the supervisor and its workers.
use dbgatlas_model::ProcessInstance;
use serde::{Deserialize, Serialize};
use std::{io, path::Path};

#[cfg(windows)]
fn creation_time(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
    if unsafe {
        GetProcessTimes(
            handle,
            &mut times[0],
            &mut times[1],
            &mut times[2],
            &mut times[3],
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
}

pub fn inspect(pid: u32) -> io::Result<ProcessInstance> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        };
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let result = creation_time(handle);
        unsafe {
            CloseHandle(handle);
        }
        Ok(ProcessInstance {
            environment_id: "local".into(),
            pid,
            creation_time_100ns: Some(result?),
        })
    }
    #[cfg(not(windows))]
    {
        Ok(ProcessInstance {
            environment_id: "local".into(),
            pid,
            creation_time_100ns: None,
        })
    }
}

pub fn is_alive(expected: &ProcessInstance) -> io::Result<bool> {
    if expected.environment_id != "local" || expected.creation_time_100ns.is_none() {
        return Err(io::Error::other("process instance unavailable"));
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZATION_SYNCHRONIZE,
                WaitForSingleObject,
            },
        };
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZATION_SYNCHRONIZE,
                0,
                expected.pid,
            )
        };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(87) {
                Ok(false)
            } else {
                Err(error)
            };
        }
        let result = creation_time(handle).and_then(|time| {
            if Some(time) != expected.creation_time_100ns {
                return Ok(false);
            }
            match unsafe { WaitForSingleObject(handle, 0) } {
                WAIT_OBJECT_0 => Ok(false),
                WAIT_TIMEOUT => Ok(true),
                _ => Err(io::Error::last_os_error()),
            }
        });
        unsafe {
            CloseHandle(handle);
        }
        result
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::other("process monitoring unsupported"))
    }
}

pub fn terminate_verified(expected: &ProcessInstance) -> io::Result<()> {
    if expected.environment_id != "local" {
        return Err(io::Error::other(
            "refusing to terminate a non-local process",
        ));
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
                SYNCHRONIZATION_SYNCHRONIZE, TerminateProcess, WaitForSingleObject,
            },
        };
        let Some(expected_time) = expected.creation_time_100ns else {
            return Err(io::Error::other(
                "process creation time unknown; refusing termination",
            ));
        };
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | SYNCHRONIZATION_SYNCHRONIZE,
                0,
                expected.pid,
            )
        };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(87) || is_alive(expected).ok() == Some(false) {
                Ok(())
            } else {
                Err(error)
            };
        }
        let result = (|| {
            // A different instance means the owned instance already ended. Never kill its replacement.
            if creation_time(handle)? != expected_time {
                return Ok(());
            }
            if unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0 {
                return Ok(());
            }
            if unsafe { TerminateProcess(handle, 1) } == 0 {
                let error = io::Error::last_os_error();
                // A job or another owner can terminate the same registered instance
                // between the zero-time wait and TerminateProcess (ERROR_ACCESS_DENIED).
                if unsafe { WaitForSingleObject(handle, 5000) } != WAIT_OBJECT_0 {
                    return Err(error);
                }
                return Ok(());
            }
            if unsafe { WaitForSingleObject(handle, 5000) } != WAIT_OBJECT_0 {
                return Err(io::Error::other(
                    "owned process did not exit after termination",
                ));
            }
            Ok(())
        })();
        unsafe {
            CloseHandle(handle);
        }
        result
    }
    #[cfg(not(windows))]
    {
        let _ = expected;
        Err(io::Error::other(
            "instance-checked termination is unavailable",
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnedProcess {
    process: ProcessInstance,
    role: String,
}
static REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn load(path: &Path) -> io::Result<Vec<OwnedProcess>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e),
    }
}
pub fn register(path: &Path, pid: u32, role: &str) -> io::Result<ProcessInstance> {
    register_instance(path, inspect(pid)?, role)
}
pub fn register_instance(
    path: &Path,
    process: ProcessInstance,
    role: &str,
) -> io::Result<ProcessInstance> {
    let _lock = REGISTRY_LOCK
        .lock()
        .map_err(|_| io::Error::other("ownership lock poisoned"))?;
    if process.environment_id != "local" || process.creation_time_100ns.is_none() {
        return Err(io::Error::other(
            "ownership registration requires a verified local process instance",
        ));
    }
    let mut entries = load(path)?;
    if !entries
        .iter()
        .any(|entry| entry.process.same_instance(&process))
    {
        entries.push(OwnedProcess {
            process: process.clone(),
            role: role.into(),
        });
        dbgatlas_worker_protocol::write_atomic_bytes(path, &serde_json::to_vec(&entries)?)?;
    }
    Ok(process)
}
pub fn release_targets(path: &Path) -> io::Result<()> {
    let _lock = REGISTRY_LOCK
        .lock()
        .map_err(|_| io::Error::other("ownership lock poisoned"))?;
    let entries: Vec<_> = load(path)?
        .into_iter()
        .filter(|entry| entry.role != "target")
        .collect();
    dbgatlas_worker_protocol::write_atomic_bytes(path, &serde_json::to_vec(&entries)?)
}
pub fn is_registered(path: &Path, process: &ProcessInstance) -> io::Result<bool> {
    Ok(load(path)?
        .iter()
        .any(|entry| entry.process.same_instance(process)))
}

/// Resume only the suspended primary thread of a newly created process whose handle is retained by the caller.
#[cfg(windows)]
pub fn resume_created_process(pid: u32) -> io::Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut more = unsafe { Thread32First(snapshot, &mut entry) };
        while more != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let result = unsafe { ResumeThread(thread) };
                unsafe {
                    CloseHandle(thread);
                }
                return if result == u32::MAX {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                };
            }
            more = unsafe { Thread32Next(snapshot, &mut entry) };
        }
        Err(io::Error::other("suspended process thread was not found"))
    })();
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

pub fn confirmed_child(child: &ProcessInstance, parent: &ProcessInstance) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
            System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
        };
        if !inspect(child.pid).is_ok_and(|p| p.same_instance(child))
            || !inspect(parent.pid).is_ok_and(|p| p.same_instance(parent))
        {
            return false;
        }
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
        let mut matched = false;
        while more != 0 {
            if entry.th32ProcessID == child.pid {
                matched = entry.th32ParentProcessID == parent.pid;
                break;
            }
            more = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        unsafe {
            CloseHandle(snapshot);
        }
        matched
            && child
                .creation_time_100ns
                .zip(parent.creation_time_100ns)
                .is_some_and(|(c, p)| c >= p)
    }
    #[cfg(not(windows))]
    {
        let _ = (child, parent);
        false
    }
}

pub fn confirmed_tool_child(path: &Path, child: &ProcessInstance) -> io::Result<bool> {
    let mut entries = load(path)?;
    if let Some(directory) = path.parent() {
        for file in std::fs::read_dir(directory)? {
            let file = file?;
            if file
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("owned-helper-") && n.ends_with(".json"))
            {
                entries.extend(load(&file.path())?);
            }
        }
    }
    Ok(entries
        .iter()
        .any(|e| e.role == "tool" && confirmed_child(child, &e.process)))
}

pub fn cleanup(path: &Path) -> io::Result<()> {
    let _lock = REGISTRY_LOCK
        .lock()
        .map_err(|_| io::Error::other("ownership lock poisoned"))?;
    let mut remaining = vec![];
    let mut errors = vec![];
    // Stop registered writers first, then enumerate their final child registries.
    for entry in load(path)? {
        if let Err(error) = terminate_verified(&entry.process) {
            errors.push(format!("{} pid {}: {error}", entry.role, entry.process.pid));
            remaining.push(entry);
        }
    }
    let mut helpers = vec![];
    if path.file_name().and_then(|p| p.to_str()) == Some("owned-processes.json") {
        if let Some(directory) = path.parent() {
            for file in std::fs::read_dir(directory)? {
                let file = file?;
                if file
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("owned-helper-") && n.ends_with(".json"))
                {
                    for entry in load(&file.path())? {
                        if let Err(error) = terminate_verified(&entry.process) {
                            errors
                                .push(format!("{} pid {}: {error}", entry.role, entry.process.pid));
                            remaining.push(entry);
                        }
                    }
                    helpers.push(file.path());
                }
            }
        }
    }
    dbgatlas_worker_protocol::write_atomic_bytes(path, &serde_json::to_vec(&remaining)?)?;
    for helper in helpers {
        dbgatlas_worker_protocol::write_atomic_bytes(&helper, b"[]")?;
    }
    if path.with_file_name("unconfirmed-launch.json").exists() {
        errors.push("a launch was interrupted before target identity could be confirmed; cleanup remains incomplete".into());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(errors.join("; ")))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn reused_pid_unknown_identity_and_foreign_environment_never_terminate_current_process() {
        let current = inspect(std::process::id()).unwrap();
        let mut reused = current.clone();
        reused.creation_time_100ns = reused.creation_time_100ns.map(|n| n + 1);
        terminate_verified(&reused).unwrap();
        let mut unknown = current.clone();
        unknown.creation_time_100ns = None;
        assert!(terminate_verified(&unknown).is_err());
        let mut foreign = current.clone();
        foreign.environment_id = "another-host".into();
        assert!(terminate_verified(&foreign).is_err());
        assert!(inspect(current.pid).unwrap().same_instance(&current));
    }
    #[test]
    fn released_target_survives_tool_cleanup() {
        use std::{
            os::windows::process::CommandExt,
            process::{Command, Stdio},
        };
        let temp = tempfile::tempdir().unwrap();
        let registry = temp.path().join("owned-processes.json");
        let spawn = || {
            Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ])
                .creation_flags(0x08000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut target = spawn();
        let mut tool = spawn();
        register(&registry, target.id(), "target").unwrap();
        register(&registry, tool.id(), "tool").unwrap();
        release_targets(&registry).unwrap();
        cleanup(&registry).unwrap();
        assert!(target.try_wait().unwrap().is_none());
        assert!(tool.try_wait().unwrap().is_some());
        target.kill().unwrap();
        target.wait().unwrap();
    }
}
