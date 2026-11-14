use super::*;

#[derive(Serialize, Deserialize)]
pub(super) struct RecordingRecovery {
    pub name: String,
    pub trace: PathBuf,
    pub previous_interval_us: u32,
    pub applied_interval_us: u32,
    pub completed: bool,
}

pub fn recover_recording_resources(directory: &Path) -> Result<(), ServiceError> {
    let path = directory.join("recovery.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut recovery: RecordingRecovery = serde_json::from_slice(&bytes)?;
    if recovery.completed {
        return Ok(());
    }
    let id = directory
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| ServiceError::Worker("invalid recovery directory".into()))?;
    let expected_name = etw_session_name(&RecordingRef::new(
        Id::new(id).map_err(|e| ServiceError::Worker(e.to_string()))?,
    ));
    if recovery.name != expected_name || recovery.trace != directory.join("trace.etl") {
        return Err(ServiceError::Worker(
            "recording recovery ownership mismatch".into(),
        ));
    }
    dbgatlas_etw::recover_session(
        &recovery.name,
        &recovery.trace,
        recovery.previous_interval_us,
        recovery.applied_interval_us,
    )
    .map_err(|e| ServiceError::Worker(e.to_string()))?;
    recovery.completed = true;
    write_atomic_bytes(&path, &serde_json::to_vec(&recovery)?)?;
    Ok(())
}

impl ProcessWorkerSupervisor {
    pub(super) fn recover_worker_recording(
        &self,
        ownership_file: &Path,
    ) -> Result<(), ServiceError> {
        let directory = ownership_file
            .parent()
            .ok_or_else(|| ServiceError::Worker("invalid ownership file".into()))?;
        if !directory.join("recovery.json").is_file() {
            return Ok(());
        }
        let exe = self
            .worker_exe
            .clone()
            .map(Ok)
            .unwrap_or_else(worker_executable_path)?;
        let child = Command::new(exe)
            .arg("--cleanup-recording")
            .arg(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut child = WorkerProcess::Std(child);
        if !child.wait_timeout(Duration::from_secs(15))? {
            child.kill()?;
            child.wait()?;
            return Err(ServiceError::Worker(
                "recording cleanup helper timed out".into(),
            ));
        }
        if child.try_wait()? != Some(Some(0)) {
            return Err(ServiceError::Worker(
                "recording cleanup helper failed; reservation retained".into(),
            ));
        }
        Ok(())
    }
}
