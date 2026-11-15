use super::*;

pub(super) fn unique_pipe_name(session_id: &SessionRef) -> String {
    unique_worker_pipe_name(session_id, "rpc")
}

pub(super) fn unique_lifetime_pipe_name(session_id: &SessionRef) -> String {
    unique_worker_pipe_name(session_id, "life")
}

pub(super) fn unique_worker_pipe_name(session_id: &SessionRef, purpose: &str) -> String {
    let counter = WORKER_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
        ^ counter as u128;
    format!(
        r"\\.\pipe\dbgatlas-{purpose}-{}-{}-{nonce:x}",
        std::process::id(),
        session_id.id.as_str()
    )
}

pub(super) fn worker_executable_path() -> Result<PathBuf, ServiceError> {
    let current = std::env::current_exe()?;
    let directory = current
        .parent()
        .ok_or_else(|| ServiceError::Worker("current executable has no parent".to_string()))?;
    let file_name = if cfg!(windows) {
        "dbgatlas-worker.exe"
    } else {
        "dbgatlas-worker"
    };
    Ok(directory.join(file_name))
}

pub(super) fn prepare_dbgeng_dirs_for_worker_identity(
    identity: &WorkerIdentity,
    dbgeng_dirs: &[PathBuf],
) -> Result<Vec<PathBuf>, ServiceError> {
    #[cfg(windows)]
    {
        if matches!(identity, WorkerIdentity::ActiveInteractiveUser) {
            return prepare_active_user_dbgeng_dirs(dbgeng_dirs, &active_user_dbgeng_cache_root());
        }
    }

    Ok(dbgeng_dirs.to_vec())
}

#[cfg(windows)]
pub(super) fn prepare_active_user_dbgeng_dirs(
    dbgeng_dirs: &[PathBuf],
    cache_root: &Path,
) -> Result<Vec<PathBuf>, ServiceError> {
    dbgeng_dirs
        .iter()
        .map(|dir| {
            if is_windowsapps_windbg_dbgeng_dir(dir) {
                match cache_windowsapps_dbgeng_dir_for_active_user(dir, cache_root) {
                    Ok(cached) => Ok(cached),
                    Err(error) => {
                        append_service_diagnostic_log(&format!(
                            "dbgeng_runtime_cache_failed source={} error={}",
                            sanitize_log_value(&dir.display().to_string()),
                            sanitize_log_value(&error.to_string())
                        ));
                        Ok(dir.clone())
                    }
                }
            } else {
                Ok(dir.clone())
            }
        })
        .collect()
}

#[cfg(windows)]
pub(super) fn active_user_dbgeng_cache_root() -> PathBuf {
    default_windows_service_paths()
        .var_dir
        .join("runtime-cache")
        .join("dbgeng")
}

#[cfg(windows)]
pub(super) fn is_windowsapps_windbg_dbgeng_dir(path: &Path) -> bool {
    let path = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    path.contains("\\windowsapps\\microsoft.windbg_")
}

#[cfg(windows)]
pub(super) fn cache_windowsapps_dbgeng_dir_for_active_user(
    source_dir: &Path,
    cache_root: &Path,
) -> Result<PathBuf, ServiceError> {
    let key = dbgeng_cache_key(source_dir);
    let destination = cache_root.join(key);
    let marker = destination.join(".dbgatlas-cache-complete");
    if marker.is_file() && destination.join("dbgeng.dll").is_file() {
        return Ok(destination);
    }

    fs::create_dir_all(cache_root)?;
    let staging_nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let staging = cache_root.join(format!(
        "{}.staging-{}-{staging_nonce:x}",
        destination
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("dbgeng"),
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }

    copy_dir_recursive(source_dir, &staging)?;
    fs::write(
        staging.join(".dbgatlas-cache-complete"),
        source_dir.display().to_string(),
    )?;
    match fs::rename(&staging, &destination) {
        Ok(()) => {}
        Err(_error) if marker.is_file() && destination.join("dbgeng.dll").is_file() => {
            let _ = fs::remove_dir_all(&staging);
            append_service_diagnostic_log(&format!(
                "dbgeng_runtime_cache_race_reused destination={}",
                sanitize_log_value(&destination.display().to_string())
            ));
            return Ok(destination);
        }
        Err(error) => return Err(error.into()),
    }
    append_service_diagnostic_log(&format!(
        "dbgeng_runtime_cached source={} destination={}",
        sanitize_log_value(&source_dir.display().to_string()),
        sanitize_log_value(&destination.display().to_string())
    ));
    Ok(destination)
}

#[cfg(windows)]
pub(super) fn dbgeng_cache_key(source_dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    source_dir
        .to_string_lossy()
        .to_ascii_lowercase()
        .hash(&mut hasher);
    let hash = hasher.finish();
    let label = source_dir
        .parent()
        .and_then(Path::file_name)
        .or_else(|| source_dir.file_name())
        .and_then(OsStr::to_str)
        .map(sanitize_cache_label)
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| "dbgeng".to_string());
    format!("{label}-{hash:016x}")
}

#[cfg(windows)]
pub(super) fn sanitize_cache_label(label: &str) -> String {
    label
        .chars()
        .take(96)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(windows)]
pub(super) fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), ServiceError> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn windbg_runtime_source_candidates(paths: &ServiceInstallPaths) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    push_unique_path(&mut candidates, paths.windbg_runtime_dir.clone());
    push_unique_path(&mut candidates, paths.legacy_windbg_runtime_dir());
    candidates
}

#[cfg(windows)]
pub(super) fn append_windbg_runtime_source_candidates(
    candidates: &mut Vec<PathBuf>,
    paths: &ServiceInstallPaths,
) {
    for candidate in windbg_runtime_source_candidates(paths) {
        push_unique_path(candidates, candidate);
    }
}

pub(super) fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

#[cfg(windows)]
pub(super) fn copy_existing_windbg_runtime_to_destination(
    candidates: &[PathBuf],
    destination_dir: &Path,
) -> Result<Option<PathBuf>, ServiceError> {
    for source_dir in candidates {
        if source_dir == destination_dir || !source_dir.is_dir() {
            continue;
        }
        if destination_dir.exists() {
            fs::remove_dir_all(destination_dir)?;
        }
        copy_dir_recursive(source_dir, destination_dir)?;
        match validate_windbg_runtime_dir(destination_dir) {
            Ok(()) => {
                append_service_diagnostic_log(&format!(
                    "windbg_runtime_preserved source={} destination={}",
                    sanitize_log_value(&source_dir.display().to_string()),
                    sanitize_log_value(&destination_dir.display().to_string())
                ));
                return Ok(Some(source_dir.clone()));
            }
            Err(error) => {
                let _ = fs::remove_dir_all(destination_dir);
                append_service_diagnostic_log(&format!(
                    "windbg_runtime_preserve_incomplete source={} destination={} error={}",
                    sanitize_log_value(&source_dir.display().to_string()),
                    sanitize_log_value(&destination_dir.display().to_string()),
                    sanitize_log_value(&error.to_string())
                ));
            }
        }
    }
    Ok(None)
}

#[cfg(windows)]
#[derive(Debug)]
pub(super) struct StagedWindbgRuntime {
    pub(super) source_dir: PathBuf,
    pub(super) staging_dir: PathBuf,
    pub(super) destination_dir: PathBuf,
}

#[cfg(windows)]
pub(super) fn prepare_store_windbg_runtime_staging(
    paths: &ServiceInstallPaths,
    suffix: &str,
) -> Result<Option<StagedWindbgRuntime>, ServiceError> {
    prepare_store_windbg_runtime_staging_for_destination(&paths.windbg_runtime_dir, suffix)
}

#[cfg(windows)]
pub(super) fn prepare_store_windbg_runtime_staging_for_destination(
    destination_dir: &Path,
    suffix: &str,
) -> Result<Option<StagedWindbgRuntime>, ServiceError> {
    let Some(source_dir) = resolve_store_windbg_dbgeng_dir() else {
        append_service_diagnostic_log("windbg_runtime_source_not_found");
        return Ok(None);
    };
    if let Err(error) = validate_windbg_runtime_dir(&source_dir) {
        append_service_diagnostic_log(&format!(
            "windbg_runtime_source_incomplete source={} error={}",
            sanitize_log_value(&source_dir.display().to_string()),
            sanitize_log_value(&error.to_string())
        ));
        return Ok(None);
    }
    let parent = destination_dir.parent().ok_or_else(|| {
        ServiceError::ServiceControl(format!(
            "WinDbg runtime destination has no parent: {}",
            destination_dir.display()
        ))
    })?;
    let staging_dir = parent.join(format!("{}.next-{suffix}", windbg_runtime_arch()));
    stage_windbg_runtime_from_source(&source_dir, destination_dir, &staging_dir).map(Some)
}

#[cfg(windows)]
pub(super) fn prepare_update_windbg_runtime_in_staging(
    paths: &ServiceInstallPaths,
    staging_bin_dir: &Path,
    suffix: &str,
) -> Result<Option<PathBuf>, ServiceError> {
    let destination_dir = staging_bin_dir
        .join(WINDOWS_SERVICE_RT_DIR)
        .join("windbg")
        .join(windbg_runtime_arch());
    if let Some(staged) =
        prepare_store_windbg_runtime_staging_for_destination(&destination_dir, suffix)?
    {
        activate_staged_windbg_runtime(&staged, suffix)?;
        return Ok(Some(destination_dir));
    }
    let candidates = windbg_runtime_source_candidates(paths);
    if copy_existing_windbg_runtime_to_destination(&candidates, &destination_dir)?.is_some() {
        return Ok(Some(destination_dir));
    }
    Ok(None)
}

#[cfg(windows)]
pub(super) fn stage_windbg_runtime_from_source(
    source_dir: &Path,
    destination_dir: &Path,
    staging_dir: &Path,
) -> Result<StagedWindbgRuntime, ServiceError> {
    if staging_dir.exists() {
        fs::remove_dir_all(staging_dir)?;
    }
    if let Some(parent) = staging_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    copy_dir_recursive(source_dir, staging_dir)?;
    validate_windbg_runtime_dir(staging_dir)?;
    fs::write(
        staging_dir.join(".dbgatlas-runtime-source"),
        source_dir.display().to_string(),
    )?;
    append_service_diagnostic_log(&format!(
        "windbg_runtime_staged source={} staging={}",
        sanitize_log_value(&source_dir.display().to_string()),
        sanitize_log_value(&staging_dir.display().to_string())
    ));
    Ok(StagedWindbgRuntime {
        source_dir: source_dir.to_path_buf(),
        staging_dir: staging_dir.to_path_buf(),
        destination_dir: destination_dir.to_path_buf(),
    })
}

#[cfg(windows)]
pub(super) fn activate_staged_windbg_runtime(
    staged: &StagedWindbgRuntime,
    suffix: &str,
) -> Result<(), ServiceError> {
    validate_windbg_runtime_dir(&staged.staging_dir)?;
    let parent = staged.destination_dir.parent().ok_or_else(|| {
        ServiceError::ServiceControl(format!(
            "WinDbg runtime destination has no parent: {}",
            staged.destination_dir.display()
        ))
    })?;
    fs::create_dir_all(parent)?;
    let old_dir = parent.join(format!("{}.old-{suffix}", windbg_runtime_arch()));
    if old_dir.exists() {
        fs::remove_dir_all(&old_dir)?;
    }
    if staged.destination_dir.exists() {
        fs::rename(&staged.destination_dir, &old_dir)?;
    }
    match fs::rename(&staged.staging_dir, &staged.destination_dir) {
        Ok(()) => {
            if old_dir.exists() {
                fs::remove_dir_all(&old_dir)?;
            }
            append_service_diagnostic_log(&format!(
                "windbg_runtime_activated source={} destination={}",
                sanitize_log_value(&staged.source_dir.display().to_string()),
                sanitize_log_value(&staged.destination_dir.display().to_string())
            ));
            Ok(())
        }
        Err(error) => {
            if old_dir.exists() && !staged.destination_dir.exists() {
                let _ = fs::rename(&old_dir, &staged.destination_dir);
            }
            Err(error.into())
        }
    }
}

#[cfg(windows)]
pub(super) fn validate_windbg_runtime_dir(dir: &Path) -> Result<(), ServiceError> {
    let required: &[&[&str]] = &[
        &["dbgeng.dll"],
        &["dbghelp.dll"],
        &["dbgmodel.dll"],
        &["ttd", "TTD.exe"],
        &["ttd", "TTDInject.exe"],
        &["ttd", "TTDLoader.dll"],
        &["ttd", "TTDRecord.dll"],
        &["ttd", "TTDRecordCPU.dll"],
        &["ttd", "TTDReplay.dll"],
        &["ttd", "TTDReplayCPU.dll"],
    ];
    let missing = required
        .iter()
        .map(|parts| {
            parts
                .iter()
                .fold(dir.to_path_buf(), |path, part| path.join(part))
        })
        .filter(|path| !path.is_file())
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(ServiceError::ServiceControl(format!(
            "WinDbg runtime is incomplete under {}: missing {}",
            dir.display(),
            missing.join(", ")
        )));
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn cleanup_windbg_runtime_update_dirs(
    paths: &ServiceInstallPaths,
) -> Result<(), ServiceError> {
    let windbg_dir = paths.rt_dir.join("windbg");
    if !windbg_dir.is_dir() {
        return Ok(());
    }
    let next_prefix = format!("{}.next-", windbg_runtime_arch());
    let old_prefix = format!("{}.old-", windbg_runtime_arch());
    for entry in fs::read_dir(&windbg_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if name.starts_with(&next_prefix) || name.starts_with(&old_prefix) {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

pub(super) fn spawn_worker_process(
    worker_exe: &Path,
    pipe_name: &str,
    lifetime_pipe_name: &str,
    session_id: &str,
    identity: &WorkerIdentity,
    dbgeng_dirs: &[PathBuf],
) -> Result<WorkerProcess, ServiceError> {
    let args = worker_process_args(pipe_name, lifetime_pipe_name, session_id, dbgeng_dirs);
    match identity {
        WorkerIdentity::ActiveInteractiveUser => {
            spawn_active_interactive_worker_process(worker_exe, &args)
        }
        WorkerIdentity::CurrentUserDevMode | WorkerIdentity::LocalSystem => Ok(WorkerProcess::Std(
            Command::new(worker_exe).args(&args).spawn()?,
        )),
    }
}

pub(super) fn worker_process_args(
    pipe_name: &str,
    lifetime_pipe_name: &str,
    session_id: &str,
    dbgeng_dirs: &[PathBuf],
) -> Vec<String> {
    let mut args = vec![
        "--pipe".to_string(),
        pipe_name.to_string(),
        "--control-pipe".to_string(),
        format!("{pipe_name}-control"),
        "--lifetime-pipe".to_string(),
        lifetime_pipe_name.to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
    ];
    for dbgeng_dir in dbgeng_dirs {
        args.push("--dbgeng-dir".to_string());
        args.push(dbgeng_dir.display().to_string());
    }
    args
}

pub(super) fn debug_target_kind(target: &DebugTarget) -> &'static str {
    match target {
        DebugTarget::File { path } if is_ttd_run_file(path) => "ttd_run_file",
        DebugTarget::File { .. } => "dump_file",
        DebugTarget::Attach { .. } => "attach",
        DebugTarget::Launch { .. } => "launch",
    }
}

pub(super) fn debug_worker_dbgeng_attempts(
    target: &DebugTarget,
    dbgeng_dirs: &[PathBuf],
) -> Vec<Vec<PathBuf>> {
    // DbgEng DLL 一旦加载到进程内就不能可靠切换版本；TTD `.run` 又常常依赖
    // Store/SDK 版本差异。因此 `.run` 每个候选目录用独立 worker 尝试，
    // 普通 dump/attach 仍把候选一次性交给 worker，避免不必要的进程 churn。
    if matches!(target, DebugTarget::File { path } if is_ttd_run_file(path))
        && dbgeng_dirs.len() > 1
    {
        return dbgeng_dirs.iter().cloned().map(|dir| vec![dir]).collect();
    }
    vec![dbgeng_dirs.to_vec()]
}

pub(super) fn is_ttd_run_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("run"))
}

#[cfg(windows)]
pub(super) fn spawn_active_interactive_worker_process(
    worker_exe: &Path,
    args: &[String],
) -> Result<WorkerProcess, ServiceError> {
    windows_active_user_process::spawn(worker_exe, args).map(WorkerProcess::RawWindows)
}

#[cfg(not(windows))]
pub(super) fn spawn_active_interactive_worker_process(
    _worker_exe: &Path,
    _args: &[String],
) -> Result<WorkerProcess, ServiceError> {
    Err(ServiceError::WorkerTransportUnsupported)
}
