use super::*;
use serde_json::json;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

#[test]
fn service_paths_are_rooted_under_dbgatlas_install_dir() {
    let root = PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas");
    let paths = ServiceInstallPaths::for_root(root.clone());

    assert_eq!(paths.root_dir, root);
    assert_eq!(
        paths.bin_dir,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin")
    );
    assert_eq!(
        paths.etc_dir,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\etc")
    );
    assert_eq!(
        paths.rt_dir,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin\rt")
    );
    assert_eq!(
        paths.windbg_runtime_dir,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin\rt\windbg")
            .join(windbg_runtime_arch())
    );
    assert_eq!(
        paths.var_dir,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\var")
    );
    assert_eq!(
        paths.log_dir,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\var\log")
    );
    assert_eq!(
        paths.installed_exe,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin\dbgatlas.exe")
    );
    assert_eq!(
        paths.config_path,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\etc\runtime.toml")
    );
    assert_eq!(
        paths.token_file,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\etc\token")
    );
}

#[test]
fn service_paths_can_be_inferred_from_installed_executable() {
    let executable =
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\bin\dbgatlas.exe");
    let paths = service_paths_from_executable_path(&executable).unwrap();

    assert_eq!(
        paths.root_dir,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas")
    );
    assert_eq!(paths.installed_exe, executable);
    assert_eq!(
        paths.config_path,
        PathBuf::from(r"C:\Users\dstars\AppData\Local\Programs\dbgatlas\etc\runtime.toml")
    );
}

#[test]
fn service_paths_can_be_inferred_from_quoted_scm_command_line_with_install_root() {
    let command_line = PathBuf::from(
        r#""C:\Users\d stars\AppData\Local\Programs\dbgatlas\bin\dbgatlas.exe" service run --windows-service --install-root "C:\Users\d stars\AppData\Local\Programs\dbgatlas" --config "C:\ignored\etc\runtime.toml" --token-file "C:\ignored\etc\token""#,
    );

    let paths = service_paths_from_scm_binary_path(&command_line).unwrap();

    assert_eq!(
        paths.root_dir,
        PathBuf::from(r"C:\Users\d stars\AppData\Local\Programs\dbgatlas")
    );
    assert_eq!(
        paths.installed_exe,
        PathBuf::from(r"C:\Users\d stars\AppData\Local\Programs\dbgatlas\bin\dbgatlas.exe")
    );
}

#[test]
fn service_paths_can_be_inferred_from_legacy_scm_config_argument() {
    let command_line = PathBuf::from(
        r#""C:\ProgramData\DbgAtlas\bin\dbgatlas.exe" service run --windows-service --config "C:\ProgramData\DbgAtlas\etc\runtime.toml" --token-file "C:\ProgramData\DbgAtlas\etc\token""#,
    );

    let paths = service_paths_from_scm_binary_path(&command_line).unwrap();

    assert_eq!(paths.root_dir, PathBuf::from(r"C:\ProgramData\DbgAtlas"));
    assert_eq!(
        paths.installed_exe,
        PathBuf::from(r"C:\ProgramData\DbgAtlas\bin\dbgatlas.exe")
    );
}

#[test]
fn service_doctor_writes_postinstall_report_and_summary() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.bin_dir).unwrap();
    fs::create_dir_all(&paths.etc_dir).unwrap();
    fs::write(&paths.config_path, "version = 1\n").unwrap();
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(paths.bin_dir.join(file_name), b"payload").unwrap();
    }
    let report_path = paths.log_dir.join(WINDOWS_SERVICE_POSTINSTALL_REPORT_FILE);
    let summary_path = paths.log_dir.join(WINDOWS_SERVICE_POSTINSTALL_SUMMARY_FILE);

    let report = run_windows_service_doctor(WindowsServiceDoctorOptions {
        install_root: Some(paths.root_dir.clone()),
        report_path: Some(report_path.clone()),
        msi_summary_path: Some(summary_path.clone()),
    })
    .unwrap();

    assert_eq!(report.install_root, paths.root_dir);
    assert_eq!(report.report_path.as_ref(), Some(&report_path));
    assert_eq!(report.msi_summary_path.as_ref(), Some(&summary_path));
    assert!(report.checks.iter().any(|check| check.id == "payload"));
    assert!(
        fs::read_to_string(report_path)
            .unwrap()
            .contains("DbgAtlas post-install diagnostic report")
    );
    assert!(
        fs::read_to_string(summary_path)
            .unwrap()
            .contains("DbgAtlas installed successfully")
    );
}

#[test]
fn doctor_msi_summary_limits_warning_count() {
    let warnings = (1..=5)
        .map(|index| format!("warning {index}"))
        .collect::<Vec<_>>();
    let report = WindowsServiceDoctorReport {
        status: DoctorCheckStatus::Warning,
        install_root: PathBuf::from(r"C:\DbgAtlas"),
        checks: warnings
            .iter()
            .map(|warning| {
                doctor_check(
                    warning,
                    warning,
                    DoctorCheckStatus::Warning,
                    warning.clone(),
                    None,
                    None,
                )
            })
            .collect(),
        warnings,
        report_path: Some(PathBuf::from(r"C:\DbgAtlas\var\log\postinstall-report.txt")),
        msi_summary_path: None,
    };

    let summary = format_doctor_msi_summary(&report);

    assert!(summary.contains("warning 1"));
    assert!(summary.contains("warning 4"));
    assert!(!summary.contains("warning 5\r\n"));
    assert!(summary.contains("1 additional warning"));
    assert!(summary.contains("postinstall-report.txt"));
}

#[test]
fn service_command_line_split_preserves_quoted_arguments() {
    let args = split_windows_command_line(
        r#""C:\Program Files\dbgatlas\dbgatlas.exe" --install-root "C:\Users\d stars\AppData\Local\Programs\dbgatlas" --empty """#,
    );

    assert_eq!(
        args,
        vec![
            r"C:\Program Files\dbgatlas\dbgatlas.exe",
            "--install-root",
            r"C:\Users\d stars\AppData\Local\Programs\dbgatlas",
            "--empty",
            "",
        ]
    );
}

#[test]
fn payload_discovery_requires_all_runtime_files() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("dbgatlas.exe"), "").unwrap();
    fs::write(temp.path().join("dbgatlas-worker.exe"), "").unwrap();

    let error = discover_service_payload(temp.path(), &temp.path().join("bin")).unwrap_err();

    assert!(matches!(error, ServiceError::IncompleteInstallPayload(_)));
    assert!(error.to_string().contains("dbgatlas_dbgeng.dll"));
}

#[test]
fn payload_discovery_maps_sources_to_destinations() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("install-bin");
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(temp.path().join(file_name), "").unwrap();
    }

    let payload = discover_service_payload(temp.path(), &destination).unwrap();

    assert_eq!(payload.len(), WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len());
    assert!(payload.iter().any(|file| {
        file.file_name == "dbgatlas-worker.exe"
            && file.destination == destination.join("dbgatlas-worker.exe")
    }));
    assert!(payload.iter().any(|file| {
        file.file_name == "dbgatlas_dbgeng.dll"
            && file.destination == destination.join("dbgatlas_dbgeng.dll")
    }));
}

#[test]
fn payload_discovery_ignores_legacy_mingw_runtime_files() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("install-bin");
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(temp.path().join(file_name), "").unwrap();
    }
    for file_name in [
        "libgcc_s_seh-1.dll",
        "libstdc++-6.dll",
        "libwinpthread-1.dll",
    ] {
        fs::write(temp.path().join(file_name), "").unwrap();
    }

    let payload = discover_service_payload(temp.path(), &destination).unwrap();

    assert_eq!(payload.len(), WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len());
    for file_name in [
        "libgcc_s_seh-1.dll",
        "libstdc++-6.dll",
        "libwinpthread-1.dll",
    ] {
        assert!(!payload.iter().any(|file| file.file_name == file_name));
    }
}

#[test]
fn use_existing_payload_validates_installed_bin_without_overwriting_files() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.bin_dir).unwrap();
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(paths.bin_dir.join(file_name), file_name).unwrap();
    }
    fs::write(paths.bin_dir.join("dbgatlas.exe"), "existing-exe").unwrap();

    let payload = validate_existing_payload(&paths).unwrap();

    assert_eq!(payload.len(), WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len());
    assert_eq!(
        fs::read_to_string(paths.bin_dir.join("dbgatlas.exe")).unwrap(),
        "existing-exe"
    );
    assert!(payload.iter().any(|file| {
        file.source == paths.bin_dir.join("dbgatlas-worker.exe")
            && file.destination == paths.bin_dir.join("dbgatlas-worker.exe")
    }));
}

#[test]
fn copy_payload_installs_files_into_bin() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("payload");
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&source).unwrap();
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(source.join(file_name), file_name).unwrap();
    }
    let payload = discover_service_payload(&source, &paths.bin_dir).unwrap();

    install_payload(&payload, &paths).unwrap();

    assert_eq!(
        fs::read_to_string(paths.bin_dir.join("dbgatlas.exe")).unwrap(),
        "dbgatlas.exe"
    );
    assert!(paths.bin_dir.join("dbgatlas_ida.dll").is_file());
}

#[test]
fn service_update_rejects_incomplete_payload() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::write(temp.path().join("dbgatlas.exe"), "").unwrap();

    let error = prepare_service_update(temp.path(), &paths, true).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("service install payload is incomplete")
    );
}

#[test]
fn service_update_rejects_installed_bin_source() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.bin_dir).unwrap();

    let error = prepare_service_update(&paths.bin_dir, &paths, true).unwrap_err();

    assert!(matches!(error, ServiceError::ServiceControl(_)));
    assert!(error.to_string().contains("installed bin directory"));
}

#[test]
fn service_update_accepted_response_maps_payload() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("payload");
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&source).unwrap();
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(source.join(file_name), file_name).unwrap();
    }

    let prepared = prepare_service_update(&source, &paths, false).unwrap();

    assert_eq!(prepared.response.status, "accepted");
    assert!(!prepared.response.restart);
    assert_eq!(prepared.response.service_name, WINDOWS_SERVICE_NAME);
    assert_eq!(prepared.response.installed_binary, paths.installed_exe);
    assert_eq!(prepared.response.log_dir, paths.log_dir);
    assert_eq!(
        prepared.response.source_dir,
        fs::canonicalize(&source).unwrap()
    );
    assert_eq!(
        prepared.response.payload.len(),
        WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES.len()
    );
    assert!(prepared.response.payload.iter().any(|file| {
        file.file_name == "dbgatlas.exe" && file.destination == paths.bin_dir.join("dbgatlas.exe")
    }));
}

#[test]
fn service_update_can_replace_runtime_config_but_not_token() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("payload");
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&paths.etc_dir).unwrap();
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(source.join(file_name), file_name).unwrap();
    }
    fs::write(
        source.join(WINDOWS_SERVICE_CONFIG_FILE),
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n",
    )
    .unwrap();
    fs::write(source.join(WINDOWS_SERVICE_TOKEN_FILE), "payload-token\n").unwrap();
    fs::write(
        &paths.config_path,
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7331\"\n",
    )
    .unwrap();
    fs::write(&paths.token_file, "installed-token\n").unwrap();

    let prepared = prepare_service_update(&source, &paths, false).unwrap();
    let config_payload = prepared
        .config_payload
        .as_ref()
        .expect("runtime config payload is discovered");

    assert_eq!(config_payload.destination, paths.config_path);
    assert!(
        prepared
            .response
            .payload
            .iter()
            .any(|file| file.destination == paths.config_path)
    );
    assert!(
        !prepared
            .response
            .payload
            .iter()
            .any(|file| file.destination == paths.token_file)
    );

    let staging = temp.path().join("config-staging");
    let staged_payload = copy_update_config_to_staging(config_payload, &staging, &paths).unwrap();
    assert!(staged_payload.source.starts_with(&staging));

    apply_update_config_payload(&staged_payload, &paths).unwrap();

    assert!(
        fs::read_to_string(&paths.config_path)
            .unwrap()
            .contains("127.0.0.1:7444")
    );
    assert_eq!(
        fs::read_to_string(&paths.token_file).unwrap(),
        "installed-token\n"
    );
}

#[test]
fn service_update_rejects_token_fields_in_runtime_config() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("payload");
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&source).unwrap();
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(source.join(file_name), file_name).unwrap();
    }
    fs::write(
        source.join(WINDOWS_SERVICE_CONFIG_FILE),
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\ntoken = \"secret\"\n",
    )
    .unwrap();

    let error = prepare_service_update(&source, &paths, false).unwrap_err();

    assert!(error.to_string().contains("must not contain token fields"));
    assert!(error.to_string().contains("server.token"));
}

#[test]
fn service_update_rejects_removed_local_debug_tool_fields() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("payload");
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&source).unwrap();
    for file_name in WINDOWS_SERVICE_REQUIRED_PAYLOAD_FILES {
        fs::write(source.join(file_name), file_name).unwrap();
    }
    fs::write(
            source.join(WINDOWS_SERVICE_CONFIG_FILE),
            "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n\n[tools]\ndbgeng_dir = \"C:\\\\stale\\\\windbg\"\n",
        )
        .unwrap();

    let error = prepare_service_update(&source, &paths, false).unwrap_err();

    assert!(error.to_string().contains("removed local debug tool field"));
    assert!(error.to_string().contains("tools.dbgeng_dir"));
}

#[test]
fn service_update_replace_helper_swaps_and_cleans_update_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    let staging = paths.root_dir.join("bin.next-test");
    fs::create_dir_all(&paths.bin_dir).unwrap();
    fs::create_dir_all(&staging).unwrap();
    fs::write(paths.bin_dir.join("dbgatlas.exe"), "old").unwrap();
    fs::write(staging.join("dbgatlas.exe"), "new").unwrap();

    let old_dir =
        replace_installed_bin_with_staging(&paths, &staging, "test", Duration::from_secs(1))
            .unwrap();

    assert_eq!(
        fs::read_to_string(paths.bin_dir.join("dbgatlas.exe")).unwrap(),
        "new"
    );
    assert_eq!(
        fs::read_to_string(old_dir.join("dbgatlas.exe")).unwrap(),
        "old"
    );
    cleanup_update_dirs(&paths).unwrap();
    assert!(paths.bin_dir.is_dir());
    assert!(!old_dir.exists());
    assert!(!staging.exists());
}

#[test]
fn detects_installing_from_installed_bin_directory() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.bin_dir).unwrap();

    assert!(source_is_installed_bin(&paths.bin_dir, &paths));
}

#[test]
fn installed_client_config_reads_runtime_config_and_token() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.bin_dir).unwrap();
    fs::create_dir_all(&paths.etc_dir).unwrap();
    fs::write(&paths.installed_exe, "").unwrap();
    fs::write(
        &paths.config_path,
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n",
    )
    .unwrap();
    fs::write(&paths.token_file, "installed-token\n").unwrap();

    let config = installed_client_config_from_paths(&paths).unwrap().unwrap();

    assert_eq!(config.bind, "127.0.0.1:7444".parse().unwrap());
    assert_eq!(config.bearer_token, "installed-token");
}

#[test]
fn install_token_prefers_system_environment_token() {
    let generated = std::cell::Cell::new(false);

    let selection = resolve_install_token(Some(" system-token \n"), Some("file-token"), || {
        generated.set(true);
        Ok("generated-token".to_string())
    })
    .unwrap();

    assert_eq!(
        selection,
        InstallTokenSelection {
            token: "system-token".to_string(),
            write_system_env: false,
        }
    );
    assert!(!generated.get());
}

#[test]
fn install_token_reuses_file_token_when_system_environment_is_missing() {
    let generated = std::cell::Cell::new(false);

    let selection = resolve_install_token(None, Some(" file-token \n"), || {
        generated.set(true);
        Ok("generated-token".to_string())
    })
    .unwrap();

    assert_eq!(
        selection,
        InstallTokenSelection {
            token: "file-token".to_string(),
            write_system_env: true,
        }
    );
    assert!(!generated.get());
}

#[test]
fn install_token_generates_token_when_system_environment_and_file_are_missing() {
    let selection =
        resolve_install_token(None, None, || Ok("generated-token".to_string())).unwrap();

    assert_eq!(
        selection,
        InstallTokenSelection {
            token: "generated-token".to_string(),
            write_system_env: true,
        }
    );
}

#[test]
fn install_token_treats_blank_values_as_missing() {
    let selection =
        resolve_install_token(
            Some(" \n"),
            Some("\t"),
            || Ok("generated-token".to_string()),
        )
        .unwrap();

    assert_eq!(
        selection,
        InstallTokenSelection {
            token: "generated-token".to_string(),
            write_system_env: true,
        }
    );
}

#[test]
fn service_capabilities_read_ida_py_eval_runtime_policy() {
    let runtime = RuntimeConfig::from_toml_str(
        r#"
version = 1

[tools.ida]
allow_py_eval = true
"#,
    )
    .unwrap();

    let capabilities = ServiceCapabilities::from_runtime_config(&runtime);

    assert!(capabilities.ida_py_eval);
}

#[test]
fn install_runtime_config_does_not_persist_auto_debug_stack() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.root_dir).unwrap();

    let runtime =
        create_runtime_config_if_missing(&paths, "127.0.0.1:7331".parse().unwrap()).unwrap();
    let config = fs::read_to_string(&paths.config_path).unwrap();

    assert_eq!(runtime.server.bind, "127.0.0.1:7331".parse().unwrap());
    assert!(!config.contains("dbgeng_dir"));
    assert!(!config.contains("ttd_dir"));
    assert!(!config.contains("WindowsApps"));
}

#[test]
fn installed_client_config_ignores_stale_config_without_installed_binary() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.etc_dir).unwrap();
    fs::write(
        &paths.config_path,
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n",
    )
    .unwrap();
    fs::write(&paths.token_file, "installed-token\n").unwrap();

    assert!(
        installed_client_config_from_paths(&paths)
            .unwrap()
            .is_none()
    );
}

#[test]
fn runtime_config_creation_rejects_non_loopback_bind() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.root_dir).unwrap();

    let error =
        create_runtime_config_if_missing(&paths, "0.0.0.0:7331".parse().unwrap()).unwrap_err();

    assert!(matches!(
        error,
        ServiceError::Runtime(dbgatlas_runtime::RuntimeConfigError::NonLoopbackBind(_))
    ));
    assert!(!paths.config_path.exists());
}

#[test]
fn install_layout_migrates_legacy_config_and_token() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.root_dir).unwrap();
    fs::write(
        paths.legacy_config_path(),
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n",
    )
    .unwrap();
    fs::write(paths.legacy_token_file(), "legacy-token\n").unwrap();

    prepare_install_layout(&paths).unwrap();

    assert!(paths.etc_dir.is_dir());
    assert!(paths.log_dir.is_dir());
    assert!(paths.install_marker_path().is_file());
    assert_eq!(
        fs::read_to_string(&paths.config_path).unwrap(),
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7444\"\n"
    );
    assert_eq!(
        fs::read_to_string(&paths.token_file).unwrap(),
        "legacy-token\n"
    );
    assert!(!paths.legacy_config_path().exists());
    assert!(!paths.legacy_token_file().exists());
}

#[test]
fn install_state_copy_preserves_previous_token_when_new_etc_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let previous = ServiceInstallPaths::for_root(temp.path().join("old"));
    let paths = ServiceInstallPaths::for_root(temp.path().join("new"));
    fs::create_dir_all(&previous.etc_dir).unwrap();
    fs::create_dir_all(&paths.etc_dir).unwrap();
    fs::write(
        &previous.config_path,
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7331\"\n",
    )
    .unwrap();
    fs::write(&previous.token_file, "previous-token\n").unwrap();

    copy_missing_install_state_from_paths(&paths, &previous).unwrap();

    assert_eq!(
        fs::read_to_string(&paths.config_path).unwrap(),
        "version = 1\n\n[server]\nbind = \"127.0.0.1:7331\"\n"
    );
    assert_eq!(
        fs::read_to_string(&paths.token_file).unwrap(),
        "previous-token\n"
    );
}

#[test]
fn cleanup_install_dirs_only_removes_files_when_purging() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    fs::create_dir_all(&paths.bin_dir).unwrap();
    fs::write(paths.bin_dir.join("dbgatlas.exe"), "payload").unwrap();

    cleanup_install_dirs(&paths, false).unwrap();

    assert!(paths.bin_dir.join("dbgatlas.exe").is_file());

    fs::write(paths.install_marker_path(), "DbgAtlas install root\n").unwrap();
    cleanup_install_dirs(&paths, true).unwrap();

    assert!(!paths.root_dir.exists());
}

#[test]
fn cleanup_install_dirs_rejects_unmarked_custom_root_when_purging() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("custom-root"));
    fs::create_dir_all(&paths.bin_dir).unwrap();
    fs::write(paths.bin_dir.join("dbgatlas.exe"), "payload").unwrap();

    let error = cleanup_install_dirs(&paths, true).unwrap_err();

    assert!(error.to_string().contains("refusing to purge install root"));
    assert!(paths.root_dir.exists());
}

#[test]
fn cleanup_install_dirs_rejects_unmarked_dbgatlas_named_root_when_purging() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("dbgatlas"));
    fs::create_dir_all(&paths.bin_dir).unwrap();
    fs::write(paths.bin_dir.join("dbgatlas.exe"), "payload").unwrap();

    let error = cleanup_install_dirs(&paths, true).unwrap_err();

    assert!(error.to_string().contains("refusing to purge install root"));
    assert!(paths.root_dir.exists());
}

#[test]
fn cleanup_install_dirs_allows_marked_custom_root_when_purging() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("custom-root"));
    fs::create_dir_all(&paths.bin_dir).unwrap();
    fs::write(paths.install_marker_path(), "DbgAtlas install root\n").unwrap();

    cleanup_install_dirs(&paths, true).unwrap();

    assert!(!paths.root_dir.exists());
}

#[cfg(windows)]
#[test]
fn service_log_pruning_keeps_seven_calendar_days() {
    let temp = tempfile::tempdir().unwrap();
    let current_day = windows_service_control::unix_day_from_date(2026, 6, 18).expect("valid date");
    for offset in 0..8 {
        let day = current_day - offset;
        let date = windows_service_control::utc_date_from_unix_day(day);
        fs::write(temp.path().join(format!("service-{date}.log")), "").unwrap();
    }
    fs::write(temp.path().join("other.log"), "").unwrap();

    windows_service_control::prune_service_logs(temp.path(), current_day).unwrap();

    assert!(temp.path().join("service-2026-06-18.log").is_file());
    assert!(temp.path().join("service-2026-06-12.log").is_file());
    assert!(!temp.path().join("service-2026-06-11.log").exists());
    assert!(temp.path().join("other.log").is_file());
}

#[cfg(windows)]
#[test]
fn service_log_file_day_rejects_invalid_dates() {
    assert!(windows_service_control::service_log_file_day("service-2026-02-29.log").is_none());
    assert!(windows_service_control::service_log_file_day("service-2024-02-29.log").is_some());
    assert!(windows_service_control::service_log_file_day("service-latest.log").is_none());
}

#[test]
fn health_rpc_returns_ok() {
    let host = ServiceHost::with_mock_workers();
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "service.health".to_string(),
        params: None,
    });

    assert!(response.error.is_none());
    assert_eq!(response.result.unwrap()["status"], "ok");
}

#[test]
fn create_session_uses_project_root_internal_dbgatlas_dir() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let response = create_debug_session(&host, temp.path());

    assert!(response.error.is_none(), "{:?}", response.error);
    assert!(
        temp.path()
            .join(INTERNAL_WORKSPACE_DIR)
            .join("dbgatlas-workspace.json")
            .is_file()
    );
    let result = response.result.unwrap();
    assert!(result.get("session_id").is_some());
    assert!(result.get("worker_binding").is_none());
    assert_eq!(result["operation_status"], "success");
    assert!(result["artifact_refs"].as_array().unwrap().is_empty());
    assert!(result["raw_output_ref"].is_null());
}

#[test]
fn debug_session_create_rejects_ttd_kind_with_file_hint() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "debug.session.create".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "kind": "ttd", "path": "trace.run" }
        })),
    });

    let error = response.error.unwrap();
    assert_eq!(error.code, -32602);
    assert!(error.message.contains("kind `ttd`"));
    assert!(error.message.contains("kind\": \"file"));
}

#[test]
fn debug_session_create_accepts_active_interactive_user_override() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(RecordingIdentitySupervisor {
        identities: Mutex::new(Vec::new()),
    });
    let host = ServiceHost::new(supervisor.clone());
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "debug.session.create".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "worker_identity": "active_interactive_user",
            "target": { "kind": "file", "path": "sample.dmp" }
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    assert_eq!(
        supervisor.identities.lock().unwrap().as_slice(),
        &[Some(WorkerIdentity::ActiveInteractiveUser)]
    );
}

#[test]
fn debug_session_create_launch_defaults_missing_args_to_empty() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "debug.session.create".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "kind": "launch", "executable": "cmd.exe" }
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    assert_eq!(response.result.unwrap()["operation_status"], "success");
}

#[test]
fn debug_session_create_access_denied_suggests_active_user_override() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(AccessDeniedStartSupervisor));
    let response = create_debug_session(&host, temp.path());

    let error = response.error.unwrap();
    assert!(
        error
            .message
            .contains("recommended_retry_worker_identity=active_interactive_user")
    );
    assert!(error.message.contains("worker_identity"));
    assert!(error.message.contains("active_interactive_user"));
}

#[test]
fn file_session_create_auto_retries_active_user_on_access_denied() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(AccessDeniedThenActiveSupervisor {
        identities: Mutex::new(Vec::new()),
    });
    let host = ServiceHost::new(supervisor.clone()).with_capabilities(ServiceCapabilities {
        dbgeng_dirs: vec![PathBuf::from(r"C:\DbgEng")],
        ..Default::default()
    });
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "debug.session.create".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "kind": "file", "path": "sample.dmp" }
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    assert_eq!(
        supervisor.identities.lock().unwrap().as_slice(),
        &[None, Some(WorkerIdentity::ActiveInteractiveUser)]
    );
}

#[test]
fn failed_session_create_is_recorded_as_failed_operation() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(FailingStartSupervisor));
    let response = create_debug_session(&host, temp.path());

    assert!(response.error.is_some());
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let operations = workspace.list_operations().unwrap();
    let operation = operations
        .iter()
        .find(|operation| operation.capability == "debug.session.create")
        .unwrap();
    assert!(matches!(operation.status, OperationStatus::Failed));
    assert!(operation.summary.contains("start_failed"));
}

#[test]
fn session_id_is_enough_after_create() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let create = create_debug_session(&host, temp.path());
    let session_id = create.result.unwrap()["session_id"].clone();

    let eval = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "command": ".echo hello"
        })),
    });

    assert!(eval.error.is_none(), "{:?}", eval.error);
    assert!(
        eval.result.unwrap()["output"]
            .as_str()
            .unwrap()
            .contains(".echo hello")
    );
}

#[test]
fn debug_eval_truncates_inline_output_and_keeps_raw_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let eval = eval_request(&host, session_id.clone(), ".mock_long_output");

    assert!(eval.error.is_none(), "{:?}", eval.error);
    let result = eval.result.unwrap();
    assert_eq!(result["output_truncated"], true);
    assert_eq!(
        result["output"].as_str().unwrap().len(),
        DEFAULT_INLINE_TEXT_BYTE_LIMIT
    );
    assert_eq!(
        result["full_output_byte_len"],
        json!(DEFAULT_INLINE_TEXT_BYTE_LIMIT + 100)
    );
    assert_eq!(result["full_output_artifact_ref"], result["raw_output_ref"]);

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let raw_ref: ArtifactRef = serde_json::from_value(result["raw_output_ref"].clone()).unwrap();
    let raw = workspace.get_artifact(&raw_ref).unwrap().unwrap();
    let raw_path = workspace
        .resolve_artifact_relative_path(raw.relative_path)
        .unwrap();
    assert_eq!(
        fs::read_to_string(raw_path).unwrap().len(),
        DEFAULT_INLINE_TEXT_BYTE_LIMIT + 100
    );

    let close = close_request(&host, session_id);
    assert!(close.error.is_none(), "{:?}", close.error);
}

#[test]
fn debug_eval_steps_runs_commands_in_order() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval_steps".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "commands": [
                ".exepath D:\\Repos\\Sangfor\\EDR\\sf3\\dump",
                ".reload /f sf3.dll",
                "!chkimg -lo 100 -d sf3"
            ]
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["steps"].as_array().unwrap().len(), 3);
    assert!(
        result["steps"][0]["output"]
            .as_str()
            .unwrap()
            .contains(".exepath")
    );

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let operations = workspace.list_operations().unwrap();
    assert!(
        operations
            .iter()
            .any(|operation| operation.capability == "debug.eval_steps")
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.capability == "debug.eval_steps.step")
            .count(),
        3
    );
}

#[test]
fn debug_eval_steps_reports_failed_step_without_running_later_steps() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(FailingEvalSupervisor));
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval_steps".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "commands": [".echo first", ".echo second"]
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["operation_status"], "failed");
    assert_eq!(result["failed_step_index"], 0);
    assert_eq!(result["steps"].as_array().unwrap().len(), 1);
    assert!(
        result["steps"][0]["error"]
            .as_str()
            .unwrap()
            .contains("mock eval failed")
    );
}

#[test]
fn debug_eval_steps_propagates_internal_step_errors() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(BadEvalWriteSupervisor));
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval_steps".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "commands": [".echo first"]
        })),
    });

    let error = response.error.unwrap();
    assert!(error.message.contains("artifact path"));
    let state = host.lock_state().unwrap();
    assert!(state.operations.values().any(|operation| {
        operation.capability == "debug.eval_steps.step"
            && operation.status == ServiceOperationStatus::Failed
    }));
    assert!(state.operations.values().any(|operation| {
        operation.capability == "debug.eval_steps"
            && operation.status == ServiceOperationStatus::Failed
    }));
}

#[test]
fn service_info_does_not_expose_worker_details() {
    let host = ServiceHost::with_mock_workers();
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "service.info".to_string(),
        params: None,
    });

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    assert!(result.get("worker_binding").is_none());
    assert!(result.get("worker_identity").is_none());
}

#[test]
fn process_worker_identity_policy_separates_dev_and_installed_service() {
    let dev = ProcessWorkerSupervisor::new().unwrap();
    assert_eq!(dev.identity, WorkerIdentity::CurrentUserDevMode);

    let installed = ProcessWorkerSupervisor::new_installed_service().unwrap();
    assert_eq!(installed.identity, WorkerIdentity::LocalSystem);
}

#[cfg(windows)]
#[test]
fn active_user_worker_caches_windowsapps_dbgeng_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp
        .path()
        .join("WindowsApps")
        .join("Microsoft.WinDbg_1.0.0.0_x64__8wekyb3d8bbwe")
        .join("amd64");
    fs::create_dir_all(source.join("ttd")).unwrap();
    fs::write(source.join("dbgeng.dll"), b"dbgeng").unwrap();
    fs::write(source.join("ttd").join("TTDReplay.dll"), b"ttd").unwrap();
    let cache_root = temp.path().join("cache");

    let cached = cache_windowsapps_dbgeng_dir_for_active_user(&source, &cache_root).unwrap();
    assert_ne!(cached, source);
    assert!(cached.join("dbgeng.dll").is_file());
    assert!(cached.join("ttd").join("TTDReplay.dll").is_file());

    let active =
        prepare_active_user_dbgeng_dirs(std::slice::from_ref(&source), &cache_root).unwrap();
    assert_ne!(active[0], source);

    let system = prepare_dbgeng_dirs_for_worker_identity(
        &WorkerIdentity::LocalSystem,
        std::slice::from_ref(&source),
    )
    .unwrap();
    assert_eq!(system[0], source);
}

#[cfg(windows)]
#[test]
fn windbg_runtime_staging_activates_dbgeng_and_ttd_together() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    let source = temp
        .path()
        .join("WindowsApps")
        .join("Microsoft.WinDbg_1.0.0.0_x64__8wekyb3d8bbwe")
        .join("amd64");
    write_complete_windbg_runtime(&source);
    let staging = paths
        .rt_dir
        .join("windbg")
        .join(format!("{}.next-test", windbg_runtime_arch()));

    let staged =
        stage_windbg_runtime_from_source(&source, &paths.windbg_runtime_dir, &staging).unwrap();
    assert!(staged.staging_dir.join("dbgeng.dll").is_file());
    assert!(
        staged
            .staging_dir
            .join("ttd")
            .join("TTDRecordCPU.dll")
            .is_file()
    );

    activate_staged_windbg_runtime(&staged, "test").unwrap();

    assert!(paths.windbg_runtime_dir.join("dbgeng.dll").is_file());
    assert!(
        paths
            .windbg_runtime_dir
            .join("ttd")
            .join("TTD.exe")
            .is_file()
    );
    assert!(!staging.exists());
}

#[cfg(windows)]
#[test]
fn service_update_stages_existing_windbg_runtime_inside_next_bin() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    write_complete_windbg_runtime(&paths.windbg_runtime_dir);
    let staging_bin = paths.root_dir.join("bin.next-test");
    fs::create_dir_all(&staging_bin).unwrap();

    let staged = prepare_update_windbg_runtime_in_staging(&paths, &staging_bin, "test").unwrap();

    let staged = staged.expect("existing runtime should be staged");
    assert_eq!(
        staged,
        staging_bin
            .join(WINDOWS_SERVICE_RT_DIR)
            .join("windbg")
            .join(windbg_runtime_arch())
    );
    assert!(staged.join("dbgeng.dll").is_file());
    assert!(staged.join("ttd").join("TTD.exe").is_file());
}

#[cfg(windows)]
#[test]
fn windbg_runtime_preserve_candidates_include_legacy_root_rt() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ServiceInstallPaths::for_root(temp.path().join("DbgAtlas"));
    write_complete_windbg_runtime(&paths.legacy_windbg_runtime_dir());
    let destination = paths
        .root_dir
        .join("bin.next-test")
        .join(WINDOWS_SERVICE_RT_DIR)
        .join("windbg")
        .join(windbg_runtime_arch());

    let copied_from = copy_existing_windbg_runtime_to_destination(
        &windbg_runtime_source_candidates(&paths),
        &destination,
    )
    .unwrap();

    assert_eq!(copied_from, Some(paths.legacy_windbg_runtime_dir()));
    assert!(destination.join("dbgeng.dll").is_file());
    assert!(destination.join("ttd").join("TTD.exe").is_file());
}

#[cfg(windows)]
#[test]
fn windbg_runtime_validation_requires_ttd_recorder_files() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("amd64");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("dbgeng.dll"), b"dbgeng").unwrap();

    let error = validate_windbg_runtime_dir(&source).unwrap_err();

    assert!(error.to_string().contains("TTD.exe"));
    assert!(error.to_string().contains("TTDRecordCPU.dll"));
}

#[cfg(windows)]
fn write_complete_windbg_runtime(root: &Path) {
    fs::create_dir_all(root.join("ttd")).unwrap();
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
    for relative in required {
        let path = relative
            .iter()
            .fold(root.to_path_buf(), |path, part| path.join(part));
        fs::write(path, b"runtime").unwrap();
    }
}

#[test]
fn mcp_tool_schemas_expose_structured_targets_and_write_items() {
    let tools = mcp_tool_descriptors(ServiceCapabilities::default());
    let debug = tools
        .iter()
        .find(|tool| tool["name"] == "debug.session.create")
        .unwrap();
    assert!(debug["description"].as_str().unwrap().contains("kind=file"));
    assert!(debug["inputSchema"]["properties"]["target"]["oneOf"].is_array());
    assert_eq!(
        debug["inputSchema"]["properties"]["target"]["oneOf"][2]["required"],
        json!(["kind", "executable"])
    );
    assert_eq!(
        debug["inputSchema"]["properties"]["worker_identity"]["enum"][1],
        "active_interactive_user"
    );
    let eval = tools
        .iter()
        .find(|tool| tool["name"] == "debug.eval")
        .unwrap();
    assert!(
        eval["description"]
            .as_str()
            .unwrap()
            .contains("IDebugControl::Execute")
    );
    assert!(
        eval["description"]
            .as_str()
            .unwrap()
            .contains("does not emulate WinDbg command-window multiline input")
    );
    assert!(
        eval["inputSchema"]["properties"]["command"]["description"]
            .as_str()
            .unwrap()
            .contains("single IDebugControl::Execute call")
    );
    assert_eq!(
        eval["inputSchema"]["properties"]["timeout_ms"]["type"],
        "integer"
    );
    let eval_steps = tools
        .iter()
        .find(|tool| tool["name"] == "debug.eval_steps")
        .unwrap();
    assert!(
        eval_steps["description"]
            .as_str()
            .unwrap()
            .contains("separate IDebugControl::Execute call")
    );
    assert!(
        eval_steps["inputSchema"]["properties"]["commands"]["description"]
            .as_str()
            .unwrap()
            .contains("Each item is executed separately")
    );
    assert_eq!(
        eval_steps["inputSchema"]["properties"]["commands"]["items"]["type"],
        "string"
    );

    let recording = tools
        .iter()
        .find(|tool| tool["name"] == "recording.ttd")
        .unwrap();
    assert!(
        recording["description"]
            .as_str()
            .unwrap()
            .contains("register the .run/.idx artifacts")
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["target"]["oneOf"][0]["required"],
        json!(["kind", "executable"])
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["target"]["oneOf"][2]["required"],
        json!(["kind", "program"])
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["target"]["oneOf"][2]["properties"]["cmd_line_filter"]
            ["type"],
        "string"
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["worker_identity"]["enum"][1],
        "active_interactive_user"
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["options"]["properties"]["record_mode"]["enum"],
        json!(["automatic", "manual"])
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["options"]["properties"]["replay_cpu_support"]["default"],
        "default"
    );

    let add_symbols = tools
        .iter()
        .find(|tool| tool["name"] == "debug.add_symbols")
        .unwrap();
    assert!(
        add_symbols["description"]
            .as_str()
            .unwrap()
            .contains(".sympath+")
    );
    assert!(
        add_symbols["inputSchema"]["properties"]["symbol_path"]["description"]
            .as_str()
            .unwrap()
            .contains("does not replace")
    );

    let open = tools
        .iter()
        .find(|tool| tool["name"] == "reverse.session.open")
        .unwrap();
    assert!(
        open["description"]
            .as_str()
            .unwrap()
            .contains("not a read-only clone")
    );

    let decompile = tools
        .iter()
        .find(|tool| tool["name"] == "reverse.decompile")
        .unwrap();
    let session_schema = &decompile["inputSchema"]["properties"]["session_id"];
    assert_eq!(session_schema["oneOf"][0]["type"], "string");
    assert_eq!(
        session_schema["oneOf"][1]["properties"]["id"]["type"],
        "string"
    );
    assert!(
        session_schema["description"]
            .as_str()
            .unwrap()
            .contains("\"session-")
    );

    let inspect_item = tools
        .iter()
        .find(|tool| tool["name"] == "reverse.inspect_item")
        .unwrap();
    assert!(
        inspect_item["description"]
            .as_str()
            .unwrap()
            .contains("without mutating")
    );

    let rename = tools
        .iter()
        .find(|tool| tool["name"] == "reverse.rename")
        .unwrap();
    assert!(
        rename["description"]
            .as_str()
            .unwrap()
            .contains("call reverse.idb_save")
    );
    assert!(
        rename["inputSchema"]["properties"]["items"]["items"]["description"]
            .as_str()
            .unwrap()
            .contains("addr or name")
    );
    assert_eq!(
        rename["inputSchema"]["properties"]["items"]["items"]["properties"]["new_name"]["type"],
        "string"
    );

    let set_type = tools
        .iter()
        .find(|tool| tool["name"] == "reverse.set_type")
        .unwrap();
    assert!(set_type["inputSchema"]["properties"]["items"]["items"]["properties"]["kind"]
            ["description"]
            .as_str()
            .unwrap()
            .contains("function declaration"));

    let idb_save = tools
        .iter()
        .find(|tool| tool["name"] == "reverse.idb_save")
        .unwrap();
    assert!(
        idb_save["description"]
            .as_str()
            .unwrap()
            .contains("save in place")
    );

    let close = tools
        .iter()
        .find(|tool| tool["name"] == "debug.session.close")
        .unwrap();
    assert!(
        close["description"]
            .as_str()
            .unwrap()
            .contains("Cooperatively close")
    );
    let kill = tools
        .iter()
        .find(|tool| tool["name"] == "debug.session.kill")
        .unwrap();
    assert!(
        kill["description"]
            .as_str()
            .unwrap()
            .contains("Forcefully terminate")
    );
}

#[test]
fn repeated_close_is_rejected_after_session_is_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let first = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.session.close".to_string(),
        params: Some(json!({ "session_id": session_id.clone() })),
    });
    assert!(first.error.is_none(), "{:?}", first.error);

    let second = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "debug.session.close".to_string(),
        params: Some(json!({ "session_id": session_id })),
    });
    assert_eq!(second.error.unwrap().code, -32013);
}

#[test]
fn eval_registers_raw_output_and_transcript_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let eval = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "command": ".echo artifacts"
        })),
    });
    assert!(eval.error.is_none(), "{:?}", eval.error);
    let result = eval.result.as_ref().unwrap();
    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["artifact_refs"].as_array().unwrap().len(), 3);
    assert!(result["raw_output_ref"].get("id").is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "debug.raw_output")
    );
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "debug.transcript")
    );
    let operations = workspace.list_operations().unwrap();
    let eval_operation = operations
        .iter()
        .find(|operation| operation.capability == "debug.eval")
        .unwrap();
    assert_eq!(eval_operation.artifacts.len(), 3);
    assert!(eval_operation.raw_output.is_some());
    assert_eq!(workspace.list_command_audit().unwrap().len(), 1);
}

#[test]
fn failed_eval_is_recorded_in_command_audit() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(FailingEvalSupervisor));
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let eval = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "command": ".echo fails"
        })),
    });
    assert!(eval.error.is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let audit = workspace.list_command_audit().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].command, ".echo fails");
    assert!(matches!(audit[0].status, OperationStatus::Failed));
}

#[test]
fn debug_eval_timeout_kills_worker_and_marks_session_error() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(200));
    let host = ServiceHost::new(supervisor);
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let eval = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "command": "TTD.Calls(\"KERNELBASE!LoadLibraryExW\")",
            "timeout_ms": 10
        })),
    });

    let error = eval.error.unwrap();
    assert!(error.message.contains("timed out after 10 ms"));

    let state = host.lock_state().unwrap();
    let session = state.sessions.values().next().unwrap();
    assert_eq!(session.state, DebugSessionState::Error);
    let operation = state
        .operations
        .values()
        .find(|operation| operation.capability == "debug.eval")
        .unwrap();
    assert_eq!(operation.status, ServiceOperationStatus::Failed);
}

#[test]
fn add_symbols_uses_distinct_operation_capability() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.add_symbols".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "symbol_path": r"srv*C:\symbols*https://msdl.microsoft.com/download/symbols",
            "reload": true
        })),
    });
    assert!(response.error.is_none(), "{:?}", response.error);

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let operations = workspace.list_operations().unwrap();
    assert!(
        operations
            .iter()
            .any(|operation| operation.capability == "debug.add_symbols")
    );
}

#[test]
fn cancel_rejects_completed_operation_without_changing_status() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let eval = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "command": ".echo done"
        })),
    });
    let operation_id = eval.result.unwrap()["operation_id"].clone();

    let cancel = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "operation.cancel".to_string(),
        params: Some(json!({ "operation_id": operation_id.clone() })),
    });
    assert_eq!(cancel.error.unwrap().code, -32014);

    let get = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "operation.get".to_string(),
        params: Some(json!({ "operation_id": operation_id })),
    });
    assert_eq!(get.result.unwrap()["status"], "success");
}

#[test]
fn cancel_marks_running_operation_as_canceled() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(InstrumentedWorkerSupervisor::wait_for_cancel());
    let host = ServiceHost::new(supervisor.clone());
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let eval_host = host.clone();
    let eval_session_id = session_id.clone();

    let eval_thread = std::thread::spawn(move || {
        eval_host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "debug.eval".to_string(),
            params: Some(json!({
                "session_id": eval_session_id,
                "command": ".echo wait"
            })),
        })
    });

    let operation_id = supervisor.wait_for_operation_id();
    let cancel = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "operation.cancel".to_string(),
        params: Some(json!({ "operation_id": operation_id.clone() })),
    });
    assert!(cancel.error.is_none(), "{:?}", cancel.error);
    let eval = eval_thread.join().unwrap();
    assert!(eval.error.is_none(), "{:?}", eval.error);

    let get = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "operation.get".to_string(),
        params: Some(json!({ "operation_id": operation_id })),
    });
    assert_eq!(get.result.unwrap()["status"], "canceled");
}

#[test]
fn eval_requests_for_one_session_are_serialized() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(100));
    let host = ServiceHost::new(supervisor.clone());
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let first_host = host.clone();
    let second_host = host.clone();
    let first_session = session_id.clone();
    let second_session = session_id;

    let first = std::thread::spawn(move || eval_request(&first_host, first_session, ".echo 1"));
    let second = std::thread::spawn(move || eval_request(&second_host, second_session, ".echo 2"));
    assert!(first.join().unwrap().error.is_none());
    assert!(second.join().unwrap().error.is_none());

    assert_eq!(supervisor.max_active.load(Ordering::SeqCst), 1);
}

#[test]
fn eval_requests_for_different_sessions_can_run_concurrently() {
    let temp_a = tempfile::tempdir().unwrap();
    let temp_b = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(150));
    let host = ServiceHost::new(supervisor.clone());
    let session_a =
        create_debug_session(&host, temp_a.path()).result.unwrap()["session_id"].clone();
    let session_b =
        create_debug_session(&host, temp_b.path()).result.unwrap()["session_id"].clone();
    let first_host = host.clone();
    let second_host = host.clone();

    let first = std::thread::spawn(move || eval_request(&first_host, session_a, ".echo a"));
    let second = std::thread::spawn(move || eval_request(&second_host, session_b, ".echo b"));
    assert!(first.join().unwrap().error.is_none());
    assert!(second.join().unwrap().error.is_none());

    assert!(supervisor.max_active.load(Ordering::SeqCst) > 1);
}

#[test]
fn kill_does_not_wait_for_running_session_request() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(InstrumentedWorkerSupervisor::wait_for_cancel());
    let host = ServiceHost::new(supervisor.clone());
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let eval_host = host.clone();
    let eval_session_id = session_id.clone();
    let eval_thread = std::thread::spawn(move || {
        eval_request(&eval_host, eval_session_id, ".echo hang-until-kill")
    });
    let _operation_id = supervisor.wait_for_operation_id();

    let kill = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "debug.session.kill".to_string(),
        params: Some(json!({ "session_id": session_id })),
    });
    assert!(kill.error.is_none(), "{:?}", kill.error);
    assert_eq!(kill.result.unwrap()["state"], "Closed");
    let _ = eval_thread.join().unwrap();
}

#[test]
fn concurrent_close_rechecks_latest_session_state() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(100));
    let host = ServiceHost::new(supervisor);
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let first_host = host.clone();
    let second_host = host.clone();
    let first_session = session_id.clone();
    let second_session = session_id;

    let first = std::thread::spawn(move || close_request(&first_host, first_session));
    let second = std::thread::spawn(move || close_request(&second_host, second_session));
    let responses = [first.join().unwrap(), second.join().unwrap()];
    let success_count = responses
        .iter()
        .filter(|response| response.error.is_none())
        .count();
    let terminal_count = responses
        .iter()
        .filter(|response| {
            response
                .error
                .as_ref()
                .is_some_and(|error| error.code == -32013)
        })
        .count();

    assert_eq!(success_count, 1);
    assert_eq!(terminal_count, 1);
}

#[test]
fn eval_after_concurrent_close_rechecks_latest_session_state() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(100));
    let host = ServiceHost::new(supervisor);
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let close_host = host.clone();
    let eval_host = host.clone();
    let close_session = session_id.clone();
    let eval_session = session_id;

    let close = std::thread::spawn(move || close_request(&close_host, close_session));
    std::thread::sleep(Duration::from_millis(10));
    let eval = std::thread::spawn(move || eval_request(&eval_host, eval_session, ".echo late"));
    assert!(close.join().unwrap().error.is_none());
    assert_eq!(eval.join().unwrap().error.unwrap().code, -32012);
}

#[test]
fn read_memory_registers_memory_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.read_memory".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "address": 4096,
            "length": 16
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    assert_eq!(result["bytes_read"], 16);
    assert_eq!(result["operation_status"], "success");
    assert!(result["memory_ref"].get("id").is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "debug.memory")
    );
    let operations = workspace.list_operations().unwrap();
    assert!(
        operations
            .iter()
            .any(|operation| operation.capability == "debug.read_memory")
    );
}

#[test]
fn reverse_lookup_records_session_lookup_and_workspace_facts() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let host = ServiceHost::with_mock_workers();

    let open = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "reverse.session.open".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "database_path": database,
            "ida_install_dir": temp.path().join("ida")
        })),
    });
    assert!(open.error.is_none(), "{:?}", open.error);
    let open_result = open.result.unwrap();
    let session_id = open_result["session_id"].clone();

    let lookup = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "reverse.lookup_function".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "runtime_address": "0x180001234",
            "runtime_module_base": "0x180000000",
            "ida_image_base": "0x140000000"
        })),
    });
    assert!(lookup.error.is_none(), "{:?}", lookup.error);
    let lookup_result = lookup.result.unwrap();
    assert_eq!(lookup_result["rva"], json!(0x1234));
    assert_eq!(lookup_result["ida_ea"], json!(0x140001234u64));
    assert_eq!(lookup_result["found"], json!(true));

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "reverse.session")
    );
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "reverse.lookup")
    );
    let operations = workspace.list_operations().unwrap();
    assert!(
        operations
            .iter()
            .any(|operation| operation.capability == "reverse.session.open")
    );
    assert!(
        operations
            .iter()
            .any(|operation| operation.capability == "reverse.lookup_function")
    );
    let session_id = serde_json::from_value::<SessionRef>(session_id).unwrap();
    assert!(
        workspace
            .root()
            .join("artifacts")
            .join("reverse_sessions")
            .join(session_id.id.as_str())
            .join("sessions")
            .is_dir()
    );
    assert!(
        workspace
            .root()
            .join("artifacts")
            .join("reverse_sessions")
            .join(session_id.id.as_str())
            .join("lookups")
            .is_dir()
    );
    assert!(
        workspace
            .root()
            .join("artifacts")
            .join("reverse_sessions")
            .join(session_id.id.as_str())
            .join("sessions")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );
    assert!(
        workspace
            .root()
            .join("artifacts")
            .join("reverse_sessions")
            .join(session_id.id.as_str())
            .join("lookups")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );
}

#[test]
fn reverse_core_functions_record_artifacts_and_operations() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers().with_ida_py_eval(true);
    let session_id = open_reverse_session(&host, temp.path());
    let calls = [
        (
            "reverse.lookup_funcs",
            json!({
                "queries": ["0x140001234", "main"],
                "runtime_module_base": 0,
                "ida_image_base": 0
            }),
        ),
        (
            "reverse.int_convert",
            json!({ "inputs": "42, 0x2a, ascii:AB" }),
        ),
        (
            "reverse.list_funcs",
            json!({ "offset": 0, "count": 2, "filter": "parse" }),
        ),
        ("reverse.list_globals", json!({ "offset": 0, "count": 10 })),
        ("reverse.list_imports", json!({ "offset": 1, "count": 1 })),
        (
            "reverse.list_strings",
            json!({ "offset": 0, "count": 2, "filter": "config" }),
        ),
        ("reverse.get_string", json!({ "addr": "0x140040000" })),
        (
            "reverse.get_bytes",
            json!({ "addr": "0x140040000", "length": 8 }),
        ),
        (
            "reverse.get_int",
            json!({ "addr": "0x140040000", "size": 4, "endian": "little" }),
        ),
        ("reverse.decompile", json!({ "addr": "0x140001234" })),
        ("reverse.disasm", json!({ "addr": "0x140001234" })),
        (
            "reverse.xrefs_to",
            json!({ "addrs": ["0x140001234", "0x140001300"] }),
        ),
        (
            "reverse.xrefs_to_field",
            json!({ "queries": "MY_STRUCT.field_0" }),
        ),
        ("reverse.callees", json!({ "addrs": ["0x140001000"] })),
        (
            "reverse.rename",
            json!({ "items": [{ "kind": "function", "addr": "0x140001000", "new_name": "dbgatlas_main" }] }),
        ),
        (
            "reverse.set_comments",
            json!({ "items": [{ "addr": "0x140001000", "text": "entry point" }] }),
        ),
        (
            "reverse.set_type",
            json!({ "items": [{ "kind": "function", "addr": "0x140001000", "type": "int dbgatlas_main(void)" }] }),
        ),
        (
            "reverse.declare_type",
            json!({ "decls": "struct DbgAtlasContext { int state; };" }),
        ),
        (
            "reverse.inspect_item",
            json!({ "queries": ["0x140001000"] }),
        ),
        (
            "reverse.force_recompile",
            json!({ "addrs": ["0x140001000"] }),
        ),
        ("reverse.idb_save", json!({})),
        (
            "reverse.find_bytes",
            json!({ "patterns": ["48 8B ?? ??"], "limit": 2 }),
        ),
        (
            "reverse.search_text",
            json!({ "query": "config", "scope": "all", "limit": 2 }),
        ),
        (
            "reverse.py_eval",
            json!({ "code": "print('hello from dbgatlas')" }),
        ),
        (
            "reverse.query_xrefs",
            json!({ "target": "0x140001000", "direction": "to", "xref_type": "all" }),
        ),
        (
            "reverse.query_funcs",
            json!({ "filter": "parse", "sort_by": "name" }),
        ),
        (
            "reverse.query_entities",
            json!({ "kind": "functions", "filter": "main" }),
        ),
    ];

    for (method, mut args) in calls {
        args["session_id"] = session_id.clone();
        let response = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: method.to_string(),
            params: Some(args),
        });
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["operation_status"], "success");
        assert!(result["artifact_refs"].as_array().unwrap().len() == 1);
        assert!(result.get("result").is_some());
        assert_eq!(
            result["function"],
            method
                .strip_prefix("reverse.")
                .expect("reverse method prefix is present")
        );
    }

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .filter(|artifact| artifact.kind == "reverse.core")
            .count()
            >= 25
    );
    let operations = workspace.list_operations().unwrap();
    for capability in [
        "reverse.lookup_funcs",
        "reverse.int_convert",
        "reverse.list_funcs",
        "reverse.list_globals",
        "reverse.list_imports",
        "reverse.list_strings",
        "reverse.get_string",
        "reverse.get_bytes",
        "reverse.get_int",
        "reverse.decompile",
        "reverse.disasm",
        "reverse.xrefs_to",
        "reverse.xrefs_to_field",
        "reverse.callees",
        "reverse.rename",
        "reverse.set_comments",
        "reverse.set_type",
        "reverse.declare_type",
        "reverse.inspect_item",
        "reverse.force_recompile",
        "reverse.idb_save",
        "reverse.find_bytes",
        "reverse.search_text",
        "reverse.py_eval",
        "reverse.query_xrefs",
        "reverse.query_funcs",
        "reverse.query_entities",
    ] {
        assert!(
            operations
                .iter()
                .any(|operation| operation.capability == capability),
            "{capability} was not recorded"
        );
    }
}

#[test]
fn reverse_session_metadata_describes_write_capability() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let _session_id = open_reverse_session(&host, temp.path());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    let session_artifact = artifacts
        .iter()
        .find(|artifact| artifact.kind == "reverse.session")
        .unwrap();
    let metadata_path = workspace.root().join(&session_artifact.relative_path);
    let metadata: Value =
        serde_json::from_str(&fs::read_to_string(metadata_path).unwrap()).unwrap();
    assert_eq!(metadata["writes_idb"], false);
    assert_eq!(metadata["open_operation_writes_idb"], false);
    assert_eq!(metadata["session_write_capable"], true);
}

#[test]
fn reverse_core_accepts_string_session_id() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());
    let session_id_text = session_id["id"].as_str().unwrap().to_string();

    let response = reverse_core_rpc(
        &host,
        "reverse.list_funcs",
        json!(session_id_text),
        json!({ "offset": 0, "count": 1 }),
    );

    assert!(response.error.is_none(), "{:?}", response.error);
    assert_eq!(response.result.unwrap()["function"], "list_funcs");
}

#[test]
fn reverse_write_results_include_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());

    let set_type = reverse_core_rpc(
        &host,
        "reverse.set_type",
        session_id.clone(),
        json!({
            "items": [{
                "kind": "global",
                "addr": "0x140020000",
                "type": "struct Descriptor *[18]"
            }]
        }),
    );
    assert!(set_type.error.is_none(), "{:?}", set_type.error);
    let set_type = set_type.result.unwrap();
    let set_item = &set_type["result"]["items"][0];
    assert_eq!(set_item["ok"], true);
    assert!(
        set_item["generated_decl"]
            .as_str()
            .unwrap()
            .contains("mock_symbol")
    );
    assert_eq!(set_item["item_state"]["item_head"], json!(0x140020000u64));

    let declare = reverse_core_rpc(
        &host,
        "reverse.declare_type",
        session_id,
        json!({
            "decls": [
                "struct GoodType { int x; };",
                "invalid declaration"
            ]
        }),
    );
    assert!(declare.error.is_none(), "{:?}", declare.error);
    let declare = declare.result.unwrap();
    assert_eq!(declare["result"]["ok"], false);
    assert_eq!(declare["result"]["changed_count"], 1);
    assert_eq!(declare["result"]["errors"], 1);
    assert_eq!(declare["result"]["items"][0]["ok"], true);
    assert_eq!(declare["result"]["items"][1]["ok"], false);
    assert!(
        declare["result"]["items"][1]["hint"]
            .as_str()
            .unwrap()
            .contains("failed to parse")
    );
}

#[test]
fn reverse_core_legacy_names_are_not_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());

    for (method, args) in [
        ("reverse.imports", json!({ "offset": 0, "count": 1 })),
        ("reverse.xref_query", json!({ "target": "0x140001000" })),
        ("reverse.func_query", json!({ "filter": "parse" })),
        ("reverse.entity_query", json!({ "kind": "functions" })),
    ] {
        let response = reverse_core_rpc(&host, method, session_id.clone(), args);
        let error = response.error.expect("legacy method is rejected");
        assert!(
            error.message.contains("unknown method"),
            "{method}: {:?}",
            error
        );
    }
}

#[test]
fn reverse_py_eval_is_disabled_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());
    let response = reverse_core_rpc(
        &host,
        "reverse.py_eval",
        session_id,
        json!({ "code": "print('should not run')" }),
    );

    let error = response.error.expect("reverse.py_eval is rejected");
    assert!(error.message.contains("disabled by runtime policy"));
}

#[test]
fn reverse_decompile_large_result_returns_preview_with_artifact_ref() {
    let artifact_id = ArtifactRef::new(Id::new("artifact-large-decompile").unwrap());
    let core = ReverseCoreFunctionResult {
        function: "decompile".to_string(),
        result: json!({
            "addr": 0x140001000u64,
            "function_name": "big_function",
            "language": "c",
            "pseudocode": "x".repeat(MAX_INLINE_REVERSE_RESULT_BYTES + 1),
        }),
        warnings: Vec::new(),
    };

    let result = reverse_core_response_result(&core, &artifact_id, 200_000).unwrap();

    assert_eq!(result["pseudocode_truncated"], true);
    assert_eq!(
        result["pseudocode"].as_str().unwrap().len(),
        MAX_INLINE_DECOMPILE_PSEUDOCODE_BYTES
    );
    assert_eq!(
        result["full_result_artifact_ref"]["id"],
        "artifact-large-decompile"
    );
    assert_eq!(result["full_result_byte_len"], 200_000);
}

#[test]
fn reverse_core_functions_cover_pagination_empty_and_invalid_input() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());

    let page = reverse_core_rpc(
        &host,
        "reverse.list_funcs",
        session_id.clone(),
        json!({ "offset": 1, "count": 2 }),
    );
    assert!(page.error.is_none(), "{:?}", page.error);
    let result = page.result.unwrap();
    assert_eq!(result["result"]["offset"], 1);
    assert_eq!(result["result"]["count"], 2);
    assert_eq!(result["result"]["total"], 4);

    let empty = reverse_core_rpc(
        &host,
        "reverse.list_funcs",
        session_id.clone(),
        json!({ "filter": "does-not-exist" }),
    );
    assert!(empty.error.is_none(), "{:?}", empty.error);
    assert_eq!(empty.result.unwrap()["result"]["total"], 0);

    let empty_strings = reverse_core_rpc(
        &host,
        "reverse.list_strings",
        session_id.clone(),
        json!({ "offset": 10, "count": 2 }),
    );
    assert!(empty_strings.error.is_none(), "{:?}", empty_strings.error);
    let result = empty_strings.result.unwrap();
    assert_eq!(result["result"]["offset"], 10);
    assert_eq!(result["result"]["count"], 0);
    assert_eq!(result["result"]["total"], 3);

    for (method, args) in [
        (
            "reverse.get_bytes",
            json!({ "addr": "0x140040000", "length": 0 }),
        ),
        (
            "reverse.get_bytes",
            json!({ "addr": "0x140040000", "length": 4097 }),
        ),
        (
            "reverse.get_int",
            json!({ "addr": "0x140040000", "size": 3 }),
        ),
    ] {
        let invalid = reverse_core_rpc(&host, method, session_id.clone(), args);
        assert!(invalid.error.is_some(), "{method} accepted invalid input");
    }

    let invalid = reverse_core_rpc(
        &host,
        "reverse.decompile",
        session_id,
        json!({ "addr": "not-an-address" }),
    );
    assert!(invalid.error.is_some());
}

#[test]
fn failed_reverse_core_function_records_adapter_error_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(FailingReverseCoreSupervisor));
    let session_id = open_reverse_session(&host, temp.path());

    let response = reverse_core_rpc(
        &host,
        "reverse.decompile",
        session_id,
        json!({ "addr": "0x140001000" }),
    );
    assert!(response.error.is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "reverse.adapter_error")
    );
    let operations = workspace.list_operations().unwrap();
    let failed = operations
        .iter()
        .find(|operation| operation.capability == "reverse.decompile")
        .unwrap();
    assert!(matches!(failed.status, OperationStatus::Failed));
}

#[test]
fn debug_and_reverse_sessions_reject_cross_capability_calls() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let debug_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let reverse_id = open_reverse_session(&host, temp.path());

    let reverse_with_debug_session = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "reverse.list_funcs".to_string(),
        params: Some(json!({
            "session_id": debug_id,
            "count": 1
        })),
    });
    assert!(reverse_with_debug_session.error.is_some());

    let debug_with_reverse_session = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "debug.eval".to_string(),
        params: Some(json!({
            "session_id": reverse_id,
            "command": ".echo wrong-domain"
        })),
    });
    assert!(debug_with_reverse_session.error.is_some());
}

#[test]
fn failed_reverse_open_records_adapter_error_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let host = ServiceHost::new(Arc::new(FailingReverseOpenSupervisor));

    let open = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "reverse.session.open".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "database_path": database
        })),
    });
    assert!(open.error.is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    let error_artifact = artifacts
        .iter()
        .find(|artifact| artifact.kind == "reverse.adapter_error")
        .expect("reverse adapter error artifact is recorded");
    let error_path = workspace
        .resolve_artifact_relative_path(&error_artifact.relative_path)
        .unwrap();
    let error_text = fs::read_to_string(error_path).unwrap();
    assert!(error_text.contains("ida_error_kind=open_database_failed"));
    assert!(error_text.contains("possible_reason=database_locked_or_unavailable"));
    let operations = workspace.list_operations().unwrap();
    let failed = operations
        .iter()
        .find(|operation| operation.capability == "reverse.session.open")
        .unwrap();
    assert!(matches!(failed.status, OperationStatus::Failed));
    assert!(
        failed
            .summary
            .contains("ida_error_kind=open_database_failed")
    );
}

#[test]
fn failed_reverse_open_records_worker_create_error_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let host = ServiceHost::new(Arc::new(FailingCreateWorkerSupervisor));

    let open = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "reverse.session.open".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "database_path": database
        })),
    });
    assert!(open.error.is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    let error_artifact = artifacts
        .iter()
        .find(|artifact| artifact.kind == "reverse.adapter_error")
        .expect("reverse worker create error artifact is recorded");
    let error_path = workspace
        .resolve_artifact_relative_path(&error_artifact.relative_path)
        .unwrap();
    let error_text = fs::read_to_string(error_path).unwrap();
    assert!(error_text.contains("failed to create reverse worker"));
    assert!(error_text.contains("os error 5"));
    let operations = workspace.list_operations().unwrap();
    let failed = operations
        .iter()
        .find(|operation| operation.capability == "reverse.session.open")
        .unwrap();
    assert!(matches!(failed.status, OperationStatus::Failed));
    assert!(failed.summary.contains("failed to create reverse worker"));
}

#[test]
fn failed_reverse_close_keeps_session_reusable_and_records_failure() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let host = ServiceHost::new(Arc::new(FailingReverseCloseSupervisor));
    let open = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "reverse.session.open".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "database_path": database
        })),
    });
    let open_result = open.result.unwrap();
    let session_id = open_result["session_id"].clone();

    let close = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "reverse.session.close".to_string(),
        params: Some(json!({ "session_id": session_id })),
    });
    assert!(close.error.is_some());

    let lookup = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "reverse.lookup_function".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "runtime_address": 0x180001000u64,
            "runtime_module_base": 0x180000000u64,
            "ida_image_base": 0x140000000u64
        })),
    });
    assert!(lookup.error.is_none(), "{:?}", lookup.error);

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let close_operation = workspace
        .list_operations()
        .unwrap()
        .into_iter()
        .find(|operation| operation.capability == "reverse.session.close")
        .unwrap();
    assert!(matches!(close_operation.status, OperationStatus::Failed));
}

#[test]
fn closing_reverse_session_removes_session_from_service_state() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());

    let close = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "reverse.session.close".to_string(),
        params: Some(json!({ "session_id": session_id.clone() })),
    });
    assert!(close.error.is_none(), "{:?}", close.error);

    let lookup = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "reverse.lookup_function".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "runtime_address": 0x180001000u64,
            "runtime_module_base": 0x180000000u64,
            "ida_image_base": 0x140000000u64
        })),
    });
    assert_eq!(lookup.error.unwrap().code, -32010);
}

#[test]
fn recording_start_creates_recording_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let response = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    assert!(result.get("recording_id").is_some());
    assert_eq!(result["state"], "running");
    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["artifact_refs"].as_array().unwrap().len(), 2);

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "recording.metadata")
    );
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "recording.events.process")
    );
    let recording_id = result["recording_id"]["id"].as_str().unwrap();
    assert!(
        workspace
            .root()
            .join("artifacts")
            .join("recordings")
            .join(recording_id)
            .join("recording.json")
            .is_file()
    );
}

#[test]
fn recording_start_records_selected_etw_preset_flags() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.start".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "type": "attach", "pid": 42 },
            "presets": ["process", "network"]
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    let recording_id = result["recording_id"]["id"].as_str().unwrap();
    let recording_json = fs::read_to_string(
        temp.path()
            .join(INTERNAL_WORKSPACE_DIR)
            .join("artifacts")
            .join("recordings")
            .join(recording_id)
            .join("recording.json"),
    )
    .unwrap();
    let metadata: Value = serde_json::from_str(&recording_json).unwrap();
    let expected_flags =
        dbgatlas_etw::EtwPresetFlags::PROCESS.bits() | dbgatlas_etw::EtwPresetFlags::NETWORK.bits();

    assert_eq!(metadata["presets"], json!(["process", "network"]));
    assert_eq!(metadata["etw_preset_flags"], json!(expected_flags));
    assert!(metadata.get("trace_consumer_error").is_some());
    assert_eq!(
        metadata["adapter"]["capabilities"]["realtime_consume"],
        true
    );
    assert_eq!(
        metadata["adapter"]["capabilities"]["event_stack_trace"],
        true
    );
    assert_eq!(metadata["stack_trace"]["requested"], true);
    assert!(metadata["stack_trace"]["enabled"].is_boolean());
    assert!(metadata["stack_trace"]["warnings"].is_array());
}

#[test]
fn recording_ttd_registers_run_trace_and_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let host = ttd_test_host(MockTtdRunnerMode::CompletedWithTrace);
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.ttd".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "kind": "attach", "pid": 42 },
            "timeout_ms": 1000,
            "options": { "accept_eula": true }
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    assert_eq!(result["state"], "completed");
    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["target_pid"], 42);
    assert!(
        result["primary_trace_path"]
            .as_str()
            .unwrap()
            .ends_with("sample.run")
    );
    assert_eq!(result["trace_paths"].as_array().unwrap().len(), 1);
    assert_eq!(result["trace_index_paths"].as_array().unwrap().len(), 1);
    assert_eq!(result["trace_artifacts"].as_array().unwrap().len(), 1);
    let trace_relative_path = result["trace_artifacts"][0]["relative_path"]
        .as_str()
        .unwrap()
        .replace('/', "\\");
    assert_eq!(
        trace_relative_path,
        format!(
            "artifacts\\recordings\\{}\\traces\\sample.run",
            result["recording_id"]["id"].as_str().unwrap()
        )
    );

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "recording.ttd.trace")
    );
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "recording.ttd.index")
    );
    let recording_id = result["recording_id"]["id"].as_str().unwrap();
    let metadata: Value = serde_json::from_str(
        &fs::read_to_string(
            workspace
                .root()
                .join("artifacts")
                .join("recordings")
                .join(recording_id)
                .join("recording.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["status"], "completed");
    assert_eq!(metadata["mode"], "attach");
    assert_eq!(metadata["traces"].as_array().unwrap().len(), 1);
}

#[test]
fn recording_ttd_attach_uses_attach_args_and_requested_identity() {
    let temp = tempfile::tempdir().unwrap();
    let (host, invocations) = ttd_test_host_and_invocations(MockTtdRunnerMode::CompletedWithTrace);
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.ttd".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "kind": "attach", "pid": 4242 },
            "timeout_ms": 1000,
            "worker_identity": "active_interactive_user"
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    assert_eq!(result["mode"], "attach");
    assert_eq!(result["worker_identity"], "active_interactive_user");
    let invocations = invocations.lock().unwrap();
    assert_eq!(invocations.len(), 1);
    assert_eq!(
        invocations[0].worker_identity,
        TtdRecorderIdentity::ActiveInteractiveUser
    );
    let args = invocations[0]
        .args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(args.contains(&"-attach".to_string()));
    assert!(args.contains(&"4242".to_string()));
    assert!(!args.contains(&"-launch".to_string()));

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let recording_id = result["recording_id"]["id"].as_str().unwrap();
    let metadata: Value = serde_json::from_str(
        &fs::read_to_string(
            workspace
                .root()
                .join("artifacts")
                .join("recordings")
                .join(recording_id)
                .join("recording.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["target"]["kind"], "attach");
    assert_eq!(metadata["target"]["pid"], 4242);
    assert_eq!(metadata["worker_identity"], "active_interactive_user");
    assert_eq!(
        metadata["adapter"]["worker_identity"],
        "active_interactive_user"
    );
}

#[test]
fn ttd_active_user_helper_args_preserve_recorder_args_after_separator() {
    let args = vec![
        OsString::from("-out"),
        OsString::from(r"C:\case\traces"),
        OsString::from("-attach"),
        OsString::from("4242"),
    ];
    let helper_args = ttd_command_helper_args(
        Path::new(r"C:\ttd\TTD.exe"),
        &args,
        Path::new(r"C:\case\recorder.stdout.txt"),
        Path::new(r"C:\case\recorder.stderr.txt"),
    );
    let text = helper_args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(text[0], "service");
    assert_eq!(text[1], "run-ttd-command");
    let separator = text.iter().position(|arg| arg == "--").unwrap();
    assert_eq!(
        &text[separator + 1..],
        ["-out", r"C:\case\traces", "-attach", "4242"]
    );
}

#[test]
fn ttd_timeout_stop_args_do_not_mix_stop_and_wait() {
    let args = ttd_stop_args(OsStr::new("4242"))
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(args, ["-stop", "4242"]);
}

#[test]
fn recording_ttd_timeout_invokes_stop_and_keeps_trace() {
    let temp = tempfile::tempdir().unwrap();
    let host = ttd_test_host(MockTtdRunnerMode::TimedOutWithTrace);
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.ttd".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "kind": "launch", "executable": "app.exe", "args": [] },
            "timeout_ms": 1000
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    assert_eq!(result["state"], "timed_out");
    assert_eq!(result["operation_status"], "success");

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(artifacts.iter().any(|artifact| {
        artifact.relative_path.ends_with("recorder-stop.stdout.txt")
            && artifact.kind == "recording.recorder_output"
    }));
}

#[test]
fn recording_ttd_without_run_trace_is_failed() {
    let temp = tempfile::tempdir().unwrap();
    let host = ttd_test_host(MockTtdRunnerMode::CompletedWithoutTrace);
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.ttd".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "kind": "attach", "pid": 42 },
            "timeout_ms": 1000
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    assert_eq!(result["state"], "failed");
    assert_eq!(result["operation_status"], "failed");
    assert!(result["error"].as_str().unwrap().contains("no .run trace"));
}

#[test]
fn recording_launch_starts_target_and_records_root_pid() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let (executable, args) = trivial_recording_command();
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.start".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": {
                "type": "launch",
                "executable": executable,
                "args": args
            },
            "presets": ["process"]
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    let root_pid = result["root_pid"].as_u64().unwrap();
    assert!(root_pid > 0);

    let recording_id = result["recording_id"]["id"].as_str().unwrap();
    let recording_json = fs::read_to_string(
        temp.path()
            .join(INTERNAL_WORKSPACE_DIR)
            .join("artifacts")
            .join("recordings")
            .join(recording_id)
            .join("recording.json"),
    )
    .unwrap();
    let metadata: Value = serde_json::from_str(&recording_json).unwrap();
    assert_eq!(metadata["mode"], "launch");
    assert_eq!(metadata["root_pid"], json!(root_pid));
}

#[test]
fn recording_stop_registers_trace_and_category_events() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
    let recording_id = start.result.unwrap()["recording_id"].clone();

    let stop = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "recording.stop".to_string(),
        params: Some(json!({ "recording_id": recording_id.clone() })),
    });

    assert!(stop.error.is_none(), "{:?}", stop.error);
    let result = stop.result.as_ref().unwrap();
    assert_eq!(result["state"], "stopped");
    assert_eq!(result["operation_status"], "success");

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "recording.trace")
    );
    for category in ["process", "thread", "image", "file", "registry", "network"] {
        let kind = format!("recording.events.{category}");
        assert!(
            artifacts.iter().any(|artifact| artifact.kind == kind),
            "missing artifact kind {kind}"
        );
    }
    let operations = workspace.list_operations().unwrap();
    assert!(
        operations
            .iter()
            .any(|operation| operation.capability == "recording.stop"
                && matches!(operation.status, OperationStatus::Success))
    );
    let recording_dir = workspace
        .root()
        .join("artifacts")
        .join("recordings")
        .join(recording_id["id"].as_str().unwrap());
    let recording_metadata: Value =
        serde_json::from_str(&fs::read_to_string(recording_dir.join("recording.json")).unwrap())
            .unwrap();
    assert!(recording_metadata["trace"]["valid_etl"].is_boolean());
    assert!(recording_metadata["trace"].get("fallback_reason").is_some());
    assert!(recording_metadata.get("event_extraction").is_some());
    assert!(
        recording_metadata["event_extraction"].is_null()
            || recording_metadata["event_extraction"]["warnings"].is_array()
    );
    assert!(recording_dir.join("trace.etl").is_file());
    assert!(recording_dir.join("events").join("network.jsonl").is_file());
    for category in ["process", "thread", "image", "file", "registry", "network"] {
        let path = recording_dir
            .join("events")
            .join(format!("{category}.jsonl"));
        let text = fs::read_to_string(path).unwrap();
        let first_line = text.lines().next().unwrap();
        let event: Value = serde_json::from_str(first_line).unwrap();
        assert_eq!(event["schema_version"], 1);
        assert_eq!(event["category"], category);
        assert!(event.get("timestamp").is_some());
        assert!(event.get("event_type").is_some());
        assert!(event.get("pid").is_some());
        assert!(event.get("tid").is_some());
        assert!(event.get("process").is_some());
        assert!(event.get("etw").is_some());
        if let Some(stack) = event.get("stack") {
            let frames = stack["frames"].as_array().unwrap();
            assert!(frames.iter().all(|frame| frame.is_string()));
        }
    }
}

#[test]
fn recording_workspace_facts_exposes_reportable_event_materials() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
    let recording_id = start.result.unwrap()["recording_id"].clone();

    let stop = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "recording.stop".to_string(),
        params: Some(json!({ "recording_id": recording_id.clone() })),
    });
    assert!(stop.error.is_none(), "{:?}", stop.error);
    let stop_operation_id =
        serde_json::from_value::<OperationRef>(stop.result.unwrap()["operation_id"].clone())
            .unwrap();

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let facts = workspace.facts().unwrap();
    assert!(facts.operations.iter().any(|operation| {
        operation.operation_id == stop_operation_id
            && operation.capability == "recording.stop"
            && matches!(operation.status, OperationStatus::Success)
    }));

    let recording_id = recording_id["id"].as_str().unwrap();
    let expected = [
        ("recording.metadata", "recording.json"),
        ("recording.trace", "trace.etl"),
        ("recording.events.process", "events/process.jsonl"),
        ("recording.events.thread", "events/thread.jsonl"),
        ("recording.events.image", "events/image.jsonl"),
        ("recording.events.file", "events/file.jsonl"),
        ("recording.events.registry", "events/registry.jsonl"),
        ("recording.events.network", "events/network.jsonl"),
    ];
    for (kind, suffix) in expected {
        let relative_path = PathBuf::from(format!("artifacts/recordings/{recording_id}/{suffix}"));
        let artifact = facts
            .artifacts
            .iter()
            .find(|artifact| {
                artifact.kind == kind
                    && artifact.relative_path == relative_path
                    && artifact.operation_id.as_ref() == Some(&stop_operation_id)
            })
            .unwrap_or_else(|| panic!("missing reportable artifact {kind} at {suffix}"));
        assert!(artifact.byte_len.unwrap_or_default() > 0);
    }

    let process_events = workspace
        .root()
        .join("artifacts")
        .join("recordings")
        .join(recording_id)
        .join("events")
        .join("process.jsonl");
    let first_line = fs::read_to_string(process_events)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let event: Value = serde_json::from_str(&first_line).unwrap();
    assert_eq!(event["recording_id"]["id"], recording_id);
    assert_eq!(event["schema_version"], 1);
    assert_eq!(event["category"], "process");
}

#[test]
fn recording_cancel_is_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
    let recording_id = start.result.unwrap()["recording_id"].clone();

    let cancel = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "recording.cancel".to_string(),
        params: Some(json!({ "recording_id": recording_id.clone() })),
    });
    assert!(cancel.error.is_none(), "{:?}", cancel.error);
    assert_eq!(cancel.result.unwrap()["operation_status"], "canceled");

    let stop = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "recording.stop".to_string(),
        params: Some(json!({ "recording_id": recording_id })),
    });
    assert_eq!(stop.error.unwrap().code, -32016);

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let operations = workspace.list_operations().unwrap();
    let cancel_operation = operations
        .iter()
        .find(|operation| operation.capability == "recording.cancel")
        .unwrap();
    assert!(matches!(cancel_operation.status, OperationStatus::Canceled));
    let artifacts = workspace.list_artifacts().unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == "recording.trace")
    );
}

#[test]
fn recording_kill_records_failed_operation_and_keeps_artifacts_in_facts() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
    let recording_id = start.result.unwrap()["recording_id"].clone();

    let kill = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "recording.kill".to_string(),
        params: Some(json!({ "recording_id": recording_id.clone() })),
    });

    assert!(kill.error.is_none(), "{:?}", kill.error);
    let result = kill.result.unwrap();
    assert_eq!(result["state"], "killed");
    assert_eq!(result["operation_status"], "failed");
    let kill_operation_id =
        serde_json::from_value::<OperationRef>(result["operation_id"].clone()).unwrap();

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let facts = workspace.facts().unwrap();
    assert!(facts.operations.iter().any(|operation| {
        operation.capability == "recording.kill"
            && matches!(operation.status, OperationStatus::Failed)
    }));
    let recording_id = recording_id["id"].as_str().unwrap();
    let recording_json = PathBuf::from(format!(
        "artifacts/recordings/{recording_id}/recording.json"
    ));
    let trace = PathBuf::from(format!("artifacts/recordings/{recording_id}/trace.etl"));
    assert!(
        facts
            .artifacts
            .iter()
            .any(|artifact| artifact.relative_path == recording_json
                && artifact.kind == "recording.metadata"
                && artifact.operation_id.as_ref() == Some(&kill_operation_id))
    );
    assert!(
        facts
            .artifacts
            .iter()
            .any(|artifact| artifact.relative_path == trace
                && artifact.kind == "recording.trace"
                && artifact.operation_id.as_ref() == Some(&kill_operation_id))
    );
    let recording_metadata: Value =
        serde_json::from_str(&fs::read_to_string(workspace.root().join(recording_json)).unwrap())
            .unwrap();
    assert_eq!(recording_metadata["state"], "killed");
    assert_eq!(
        recording_metadata["operation_id"],
        serde_json::to_value(kill_operation_id).unwrap()
    );
}

#[test]
fn recording_status_uses_public_shape() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
    let recording_id = start.result.unwrap()["recording_id"].clone();

    let status = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "recording.status".to_string(),
        params: Some(json!({ "recording_id": recording_id })),
    });

    assert!(status.error.is_none(), "{:?}", status.error);
    let result = status.result.unwrap();
    assert_eq!(result["state"], "running");
    assert!(result.get("artifact_refs").is_some());
    assert!(result.get("last_operation").is_some());
    assert!(result.get("project_root").is_none());
    assert!(result.get("internal_workspace_root").is_none());
    assert!(result.get("artifact_dir").is_none());
}

#[test]
fn repeated_recording_stop_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
    let recording_id = start.result.unwrap()["recording_id"].clone();

    let first = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "recording.stop".to_string(),
        params: Some(json!({ "recording_id": recording_id.clone() })),
    });
    assert!(first.error.is_none(), "{:?}", first.error);

    let second = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "recording.stop".to_string(),
        params: Some(json!({ "recording_id": recording_id })),
    });
    assert_eq!(second.error.unwrap().code, -32016);
}

#[test]
fn concurrent_recording_finish_allows_one_terminal_operation() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let start = create_recording(&host, temp.path(), json!({ "type": "attach", "pid": 42 }));
    let recording_id = start.result.unwrap()["recording_id"].clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let stop_host = host.clone();
    let cancel_host = host.clone();
    let stop_recording = recording_id.clone();
    let cancel_recording = recording_id;
    let stop_barrier = barrier.clone();
    let cancel_barrier = barrier;

    let stop = std::thread::spawn(move || {
        stop_barrier.wait();
        stop_host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: "recording.stop".to_string(),
            params: Some(json!({ "recording_id": stop_recording })),
        })
    });
    let cancel = std::thread::spawn(move || {
        cancel_barrier.wait();
        cancel_host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: "recording.cancel".to_string(),
            params: Some(json!({ "recording_id": cancel_recording })),
        })
    });
    let responses = [stop.join().unwrap(), cancel.join().unwrap()];
    let success_count = responses
        .iter()
        .filter(|response| response.error.is_none())
        .count();
    let terminal_reject_count = responses
        .iter()
        .filter(|response| {
            response
                .error
                .as_ref()
                .is_some_and(|error| error.code == -32016)
        })
        .count();

    assert_eq!(success_count, 1);
    assert_eq!(terminal_reject_count, 1);
}

#[test]
fn rejects_missing_bearer_token() {
    let request = HttpRequest {
        method: "POST".to_string(),
        path: "/rpc".to_string(),
        headers: HashMap::new(),
        body: Vec::new(),
    };
    let error = authorize_http_request(&request, &ServiceConfig::dev_default()).unwrap_err();
    assert!(matches!(error, ServiceError::Unauthorized));
}

#[test]
fn validates_loopback_service_bind() {
    let config = ServiceConfig {
        bind: "0.0.0.0:7331".parse().unwrap(),
        bearer_token: "token".to_string(),
    };

    assert!(matches!(
        validate_config(&config),
        Err(ServiceError::NonLoopbackBind(_))
    ));
}

#[test]
fn origin_check_requires_exact_loopback_host() {
    assert!(is_allowed_origin("http://localhost:7331"));
    assert!(is_allowed_origin("http://127.0.0.1:7331"));
    assert!(is_allowed_origin("http://[::1]:7331"));
    assert!(!is_allowed_origin("http://localhost.evil.test"));
    assert!(!is_allowed_origin("http://127.0.0.1.evil.test"));
    assert!(!is_allowed_origin("http://[::1].evil.test"));
}

#[test]
fn http_mcp_initialize_returns_capabilities() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
        Some("test-token"),
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 200);
    let body = http_body_json(&response);
    assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(body["result"]["serverInfo"]["name"], "dbgatlas-mcp");
    assert!(body["result"]["capabilities"].get("tools").is_some());
    server.stop();
}

#[test]
fn http_mcp_tools_list_returns_current_tools() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
        Some("test-token"),
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 200);
    let body = http_body_json(&response);
    let tools = body["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|tool| tool["name"] == "debug.eval"));
    assert!(tools.iter().any(|tool| tool["name"] == "session.list"));
    let service_update = tools
        .iter()
        .find(|tool| tool["name"] == "service.update")
        .expect("service.update tool is listed");
    assert_eq!(
        service_update["inputSchema"]["properties"]["source_dir"]["type"],
        "string"
    );
    assert_eq!(
        service_update["inputSchema"]["properties"]["restart"]["default"],
        true
    );
    for tool_name in [
        "reverse.lookup_funcs",
        "reverse.int_convert",
        "reverse.list_funcs",
        "reverse.list_globals",
        "reverse.list_imports",
        "reverse.list_strings",
        "reverse.get_string",
        "reverse.get_bytes",
        "reverse.get_int",
        "reverse.decompile",
        "reverse.disasm",
        "reverse.xrefs_to",
        "reverse.xrefs_to_field",
        "reverse.callees",
        "reverse.rename",
        "reverse.set_comments",
        "reverse.set_type",
        "reverse.declare_type",
        "reverse.inspect_item",
        "reverse.force_recompile",
        "reverse.idb_save",
        "reverse.find_bytes",
        "reverse.search_text",
        "reverse.query_xrefs",
        "reverse.query_funcs",
        "reverse.query_entities",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == tool_name),
            "{tool_name} tool is listed"
        );
    }
    for legacy_tool_name in [
        "reverse.imports",
        "reverse.xref_query",
        "reverse.func_query",
        "reverse.entity_query",
    ] {
        assert!(
            !tools.iter().any(|tool| tool["name"] == legacy_tool_name),
            "{legacy_tool_name} legacy tool name is not listed"
        );
    }
    assert!(tools.iter().any(|tool| tool["name"] == "workspace.facts"));
    assert!(tools.iter().any(|tool| tool["name"] == "recording.ttd"));
    assert!(!tools.iter().any(|tool| tool["name"] == "reverse.py_eval"));
    assert!(!tools.iter().any(|tool| tool["name"] == "recording.start"));
    server.stop();
}

#[test]
fn http_mcp_tools_list_includes_py_eval_when_enabled() {
    let server =
        start_mock_http_service_with_host(ServiceHost::with_mock_workers().with_ida_py_eval(true));
    let response = post_json(
        server.endpoint,
        "/mcp",
        Some("test-token"),
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 200);
    let body = http_body_json(&response);
    let tools = body["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|tool| tool["name"] == "reverse.py_eval"));
    server.stop();
}

#[test]
fn http_mcp_py_eval_call_is_disabled_by_default() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
        Some("test-token"),
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "reverse.py_eval",
                "arguments": {
                    "session_id": { "kind": "reverse", "id": "reverse-1" },
                    "code": "print('should not run')"
                }
            }
        }),
    );

    assert_eq!(http_status(&response), 200);
    let body = http_body_json(&response);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("disabled by runtime policy")
    );
    server.stop();
}

#[test]
fn http_mcp_ping_returns_empty_result() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
        Some("test-token"),
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 200);
    assert_eq!(http_body_json(&response)["result"], json!({}));
    server.stop();
}

#[test]
fn http_mcp_notifications_return_accepted_without_body() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
        Some("test-token"),
        None,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 202);
    assert!(response.ends_with("\r\n\r\n"));
    server.stop();
}

#[test]
fn http_mcp_debug_workflow_uses_service_results_with_refs() {
    let temp = tempfile::tempdir().unwrap();
    let server = start_mock_http_service();
    let create = mcp_tool_call(
        server.endpoint,
        "debug.session.create",
        json!({
            "project_root": temp.path(),
            "target": { "kind": "file", "path": "sample.dmp" }
        }),
    );
    let session_id = create["session_id"].clone();
    let eval = mcp_tool_call(
        server.endpoint,
        "debug.eval",
        json!({
            "session_id": session_id,
            "command": ".echo from-http-mcp"
        }),
    );

    assert_eq!(eval["operation_status"], "success");
    assert!(eval["raw_output_ref"].get("id").is_some());
    assert_eq!(eval["artifact_refs"].as_array().unwrap().len(), 3);
    server.stop();
}

#[test]
fn http_mcp_reverse_core_function_uses_service_results_with_refs() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let server = start_mock_http_service();
    let create = mcp_tool_call(
        server.endpoint,
        "debug.session.create",
        json!({
            "project_root": temp.path(),
            "target": { "kind": "file", "path": "sample.dmp" }
        }),
    );
    assert!(create.get("session_id").is_some());
    let open = mcp_tool_call(
        server.endpoint,
        "reverse.session.open",
        json!({
            "project_root": temp.path(),
            "database_path": database
        }),
    );
    let result = mcp_tool_call(
        server.endpoint,
        "reverse.list_funcs",
        json!({
            "session_id": open["session_id"],
            "offset": 0,
            "count": 1
        }),
    );

    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["function"], "list_funcs");
    assert_eq!(result["artifact_refs"].as_array().unwrap().len(), 1);
    assert_eq!(result["result"]["count"], 1);
    server.stop();
}

#[test]
fn http_mcp_session_list_returns_debug_and_reverse_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let server = start_mock_http_service();
    let debug = mcp_tool_call(
        server.endpoint,
        "debug.session.create",
        json!({
            "project_root": temp.path(),
            "target": { "kind": "file", "path": "sample.dmp" }
        }),
    );
    let reverse = mcp_tool_call(
        server.endpoint,
        "reverse.session.open",
        json!({
            "project_root": temp.path(),
            "database_path": database
        }),
    );

    let list = mcp_tool_call(server.endpoint, "session.list", json!({}));
    let sessions = list["sessions"].as_array().unwrap();
    let debug_session = sessions
        .iter()
        .find(|session| session["session_id"] == debug["session_id"])
        .expect("debug session is listed");
    let reverse_session = sessions
        .iter()
        .find(|session| session["session_id"] == reverse["session_id"])
        .expect("reverse session is listed");

    assert_eq!(list["count"], 2);
    assert_eq!(debug_session["capability"], "debug");
    assert_eq!(debug_session["state"], "Ready");
    assert_eq!(debug_session["close_tool"], "debug.session.close");
    assert_eq!(debug_session["kill_tool"], "debug.session.kill");
    assert!(debug_session["target"].is_object());
    assert_eq!(reverse_session["capability"], "reverse");
    assert_eq!(reverse_session["state"], "Ready");
    assert_eq!(reverse_session["close_tool"], "reverse.session.close");
    assert!(reverse_session["kill_tool"].is_null());
    assert!(
        reverse_session["database_path"]
            .as_str()
            .unwrap()
            .ends_with("sample.exe")
    );
    server.stop();
}

#[test]
fn http_mcp_reverse_query_funcs_sort_by_returns_tool_result() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let server = start_mock_http_service();
    let open = mcp_tool_call(
        server.endpoint,
        "reverse.session.open",
        json!({
            "project_root": temp.path(),
            "database_path": database
        }),
    );
    let result = mcp_tool_call(
        server.endpoint,
        "reverse.query_funcs",
        json!({
            "session_id": open["session_id"],
            "filter": "parse",
            "sort_by": "name",
            "offset": 0,
            "count": 20
        }),
    );

    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["function"], "query_funcs");
    assert_eq!(result["result"]["items"][0]["name"], "parse_args");
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    assert!(
        workspace
            .list_operations()
            .unwrap()
            .iter()
            .any(|operation| {
                operation.capability == "reverse.query_funcs"
                    && matches!(operation.status, OperationStatus::Success)
            })
    );
    server.stop();
}

#[test]
fn http_mcp_reverse_core_error_is_tool_error() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let server =
        start_mock_http_service_with_host(ServiceHost::new(Arc::new(FailingReverseCoreSupervisor)));
    let open = mcp_tool_call(
        server.endpoint,
        "reverse.session.open",
        json!({
            "project_root": temp.path(),
            "database_path": database
        }),
    );
    let body = mcp_tool_call_body(
        server.endpoint,
        "reverse.decompile",
        json!({
            "session_id": open["session_id"],
            "addr": "0x140001000"
        }),
    );

    assert_eq!(body["result"]["isError"], true, "{body}");
    let text: Value =
        serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["error"]["code"], -32000);
    assert!(
        text["error"]["message"]
            .as_str()
            .unwrap()
            .contains("mock IDA core function failed")
    );
    assert!(
        text["operation_id"]["id"]
            .as_str()
            .unwrap()
            .starts_with("op-")
    );
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    assert!(
        workspace
            .list_artifacts()
            .unwrap()
            .iter()
            .any(|artifact| artifact.kind == "reverse.adapter_error")
    );
    assert!(
        workspace
            .list_operations()
            .unwrap()
            .iter()
            .any(|operation| {
                operation.capability == "reverse.decompile"
                    && matches!(operation.status, OperationStatus::Failed)
            })
    );
    server.stop();
}

#[test]
fn http_mcp_workspace_facts_reads_recording_layer() {
    let temp = tempfile::tempdir().unwrap();
    let server = start_mock_http_service();
    let create = mcp_tool_call(
        server.endpoint,
        "debug.session.create",
        json!({
            "project_root": temp.path(),
            "target": { "kind": "file", "path": "sample.dmp" }
        }),
    );
    let session_id = create["session_id"].clone();
    mcp_tool_call(
        server.endpoint,
        "debug.eval",
        json!({
            "session_id": session_id,
            "command": ".echo facts"
        }),
    );

    let facts = mcp_tool_call(
        server.endpoint,
        "workspace.facts",
        json!({ "path": temp.path().join(INTERNAL_WORKSPACE_DIR) }),
    );

    assert_eq!(facts["command_audit"].as_array().unwrap().len(), 1);
    assert!(
        facts["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation["capability"] == "debug.eval")
    );
    server.stop();
}

#[test]
fn workspace_facts_falls_back_to_project_artifacts_without_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let trace = temp
        .path()
        .join(INTERNAL_WORKSPACE_DIR)
        .join("artifacts")
        .join("recordings")
        .join("recording-1")
        .join("traces")
        .join("sample.run");
    fs::create_dir_all(trace.parent().unwrap()).unwrap();
    fs::write(&trace, b"run").unwrap();

    let facts = workspace_facts_with_fallback(temp.path()).unwrap();

    let artifact = facts
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "recording.ttd.trace")
        .unwrap();
    assert_eq!(artifact.byte_len, Some(3));
    assert!(
        artifact
            .artifact_id
            .id
            .as_str()
            .starts_with("artifact-synthetic-")
    );
}

#[test]
fn http_mcp_rejects_missing_bearer_token() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
        None,
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 401);
    assert_eq!(http_body_json(&response)["error"]["code"], -32001);
    server.stop();
}

#[test]
fn http_mcp_rejects_non_loopback_origin() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
        Some("test-token"),
        Some("http://localhost.evil.test"),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 403);
    assert_eq!(http_body_json(&response)["error"]["code"], -32002);
    server.stop();
}

#[test]
fn http_rpc_path_still_returns_health() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/rpc",
        Some("test-token"),
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "service.health",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 200);
    assert_eq!(http_body_json(&response)["result"]["status"], "ok");
    server.stop();
}

struct TestServer {
    endpoint: SocketAddr,
    shutdown: ServiceShutdown,
    handle: std::thread::JoinHandle<Result<(), ServiceError>>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl TestServer {
    fn stop(self) {
        self.shutdown.request_stop();
        self.handle.join().unwrap().unwrap();
    }
}

fn start_mock_http_service() -> TestServer {
    start_mock_http_service_with_host(ServiceHost::with_mock_workers())
}

fn start_mock_http_service_with_host(host: ServiceHost) -> TestServer {
    static HTTP_TEST_LOCK: Mutex<()> = Mutex::new(());
    let guard = HTTP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let endpoint = unused_loopback_endpoint();
    let shutdown = ServiceShutdown::new();
    let server_shutdown = shutdown.clone();
    let handle = std::thread::spawn(move || {
        run_http_service_until(
            ServiceConfig {
                bind: endpoint,
                bearer_token: "test-token".to_string(),
            },
            host,
            server_shutdown,
        )
    });
    wait_for_service(endpoint);
    TestServer {
        endpoint,
        shutdown,
        handle,
        _guard: guard,
    }
}

fn unused_loopback_endpoint() -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = listener.local_addr().unwrap();
    drop(listener);
    endpoint
}

fn wait_for_service(endpoint: SocketAddr) {
    for _ in 0..50 {
        if TcpStream::connect(endpoint).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("service did not start at {endpoint}");
}

fn post_json(
    endpoint: SocketAddr,
    path: &str,
    token: Option<&str>,
    origin: Option<&str>,
    body: Value,
) -> String {
    let body = serde_json::to_string(&body).unwrap();
    let mut stream = TcpStream::connect(endpoint).unwrap();
    write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: {endpoint}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        )
        .unwrap();
    if let Some(token) = token {
        write!(stream, "Authorization: Bearer {token}\r\n").unwrap();
    }
    if let Some(origin) = origin {
        write!(stream, "Origin: {origin}\r\n").unwrap();
    }
    write!(stream, "\r\n{body}").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn http_status(response: &str) -> u16 {
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .unwrap()
}

fn http_body_json(response: &str) -> Value {
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

fn mcp_tool_call_body(endpoint: SocketAddr, name: &str, arguments: Value) -> Value {
    let response = post_json(
        endpoint,
        "/mcp",
        Some("test-token"),
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }),
    );
    assert_eq!(http_status(&response), 200);
    http_body_json(&response)
}

fn mcp_tool_call(endpoint: SocketAddr, name: &str, arguments: Value) -> Value {
    let body = mcp_tool_call_body(endpoint, name, arguments);
    assert_eq!(body["result"]["isError"], false, "{body}");
    serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn create_debug_session(host: &ServiceHost, project_root: &Path) -> JsonRpcResponse {
    host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "debug.session.create".to_string(),
        params: Some(json!({
            "project_root": project_root,
            "target": { "kind": "file", "path": "sample.dmp" }
        })),
    })
}

fn open_reverse_session(host: &ServiceHost, project_root: &Path) -> Value {
    let database = project_root.join("sample.exe");
    fs::write(&database, b"sample").unwrap();
    let open = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "reverse.session.open".to_string(),
        params: Some(json!({
            "project_root": project_root,
            "database_path": database
        })),
    });
    assert!(open.error.is_none(), "{:?}", open.error);
    let result = open.result.unwrap();
    result["session_id"].clone()
}

fn reverse_core_rpc(
    host: &ServiceHost,
    method: &str,
    session_id: Value,
    mut arguments: Value,
) -> JsonRpcResponse {
    arguments["session_id"] = session_id;
    host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: method.to_string(),
        params: Some(arguments),
    })
}

fn create_recording(host: &ServiceHost, project_root: &Path, target: Value) -> JsonRpcResponse {
    host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.start".to_string(),
        params: Some(json!({
            "project_root": project_root,
            "target": target
        })),
    })
}

#[cfg(windows)]
fn trivial_recording_command() -> (PathBuf, Vec<String>) {
    (
        PathBuf::from(r"C:\Windows\System32\cmd.exe"),
        vec!["/C".to_string(), "exit 0".to_string()],
    )
}

#[cfg(not(windows))]
fn trivial_recording_command() -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("/bin/sh"),
        vec!["-c".to_string(), "exit 0".to_string()],
    )
}

fn eval_request(host: &ServiceHost, session_id: Value, command: &str) -> JsonRpcResponse {
    host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.eval".to_string(),
        params: Some(json!({
            "session_id": session_id,
            "command": command
        })),
    })
}

fn close_request(host: &ServiceHost, session_id: Value) -> JsonRpcResponse {
    host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "debug.session.close".to_string(),
        params: Some(json!({ "session_id": session_id })),
    })
}

#[derive(Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum MockTtdRunnerMode {
    CompletedWithTrace,
    TimedOutWithTrace,
    CompletedWithoutTrace,
}

struct MockTtdRunner {
    mode: MockTtdRunnerMode,
    invocations: Arc<Mutex<Vec<TtdRecorderInvocation>>>,
    _temp: tempfile::TempDir,
}

impl TtdRecorderRunner for MockTtdRunner {
    fn run(&self, invocation: TtdRecorderInvocation) -> Result<TtdProcessExit, ServiceError> {
        self.invocations.lock().unwrap().push(invocation.clone());
        fs::write(
            &invocation.stdout_path,
            "Recording process sample.exe (42)\n",
        )?;
        fs::write(&invocation.stderr_path, "")?;
        let timed_out = matches!(self.mode, MockTtdRunnerMode::TimedOutWithTrace);
        if matches!(
            self.mode,
            MockTtdRunnerMode::CompletedWithTrace | MockTtdRunnerMode::TimedOutWithTrace
        ) {
            let traces_dir = ttd_out_dir(&invocation.args);
            fs::create_dir_all(&traces_dir)?;
            fs::write(traces_dir.join("sample.run"), b"run")?;
            fs::write(traces_dir.join("sample.idx"), b"idx")?;
        }
        let stop = if timed_out {
            let timeout_stop = invocation
                .timeout_stop
                .as_ref()
                .expect("timeout invocation has stop target");
            fs::write(&timeout_stop.stdout_path, "stopped\n")?;
            fs::write(&timeout_stop.stderr_path, "")?;
            Some(TtdStopExit {
                stop_target: timeout_stop.stop_target.clone(),
                exit_code: Some(0),
                timed_out: false,
                error: None,
            })
        } else {
            assert!(invocation.timeout_stop.is_some());
            None
        };
        Ok(TtdProcessExit {
            exit_code: Some(0),
            timed_out,
            stop,
            killed_after_timeout: false,
        })
    }
}

fn ttd_test_host(mode: MockTtdRunnerMode) -> ServiceHost {
    ttd_test_host_and_invocations(mode).0
}

fn ttd_test_host_and_invocations(
    mode: MockTtdRunnerMode,
) -> (ServiceHost, Arc<Mutex<Vec<TtdRecorderInvocation>>>) {
    let temp = tempfile::tempdir().unwrap();
    let ttd_dir = temp.path().join("ttd");
    fs::create_dir_all(&ttd_dir).unwrap();
    fs::write(ttd_dir.join("TTD.exe"), b"fake").unwrap();
    let invocations = Arc::new(Mutex::new(Vec::new()));
    let host = ServiceHost::with_mock_workers()
        .with_capabilities(ServiceCapabilities {
            ttd_dir: Some(ttd_dir),
            ..Default::default()
        })
        .with_ttd_runner(Arc::new(MockTtdRunner {
            mode,
            invocations: invocations.clone(),
            _temp: temp,
        }));
    (host, invocations)
}

fn ttd_out_dir(args: &[OsString]) -> PathBuf {
    args.windows(2)
        .find_map(|pair| (pair[0] == "-out").then(|| PathBuf::from(&pair[1])))
        .expect("TTD args contain -out")
}

struct RecordingIdentitySupervisor {
    identities: Mutex<Vec<Option<WorkerIdentity>>>,
}

struct AccessDeniedStartSupervisor;

struct AccessDeniedThenActiveSupervisor {
    identities: Mutex<Vec<Option<WorkerIdentity>>>,
}

struct InstrumentedWorkerSupervisor {
    active: AtomicU64,
    max_active: AtomicU64,
    delay_ms: u64,
    wait_for_cancel: bool,
    canceled: AtomicBool,
    operation_id: Mutex<Option<OperationRef>>,
}

struct FailingCreateWorkerSupervisor;

struct FailingStartSupervisor;

struct FailingEvalSupervisor;

struct BadEvalWriteSupervisor;

struct FailingReverseOpenSupervisor;

struct FailingReverseCloseSupervisor;

struct FailingReverseCoreSupervisor;

impl WorkerSupervisor for RecordingIdentitySupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        self.identities
            .lock()
            .unwrap()
            .push(request.identity.clone());
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
            session_id: request.session_id,
            pipe_name: "test-pipe".to_string(),
            identity: request
                .identity
                .unwrap_or(WorkerIdentity::CurrentUserDevMode),
        })
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        mock_worker_response(request)
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for AccessDeniedStartSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(test_worker_handle(request.session_id))
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::StartDebugSession { .. } => Ok(WorkerResponse::Failed {
                code: "start_failed".to_string(),
                message:
                    "native call failed with status 500: HRESULT 0x80070005: Access is denied."
                        .to_string(),
                writes: Vec::new(),
            }),
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for AccessDeniedThenActiveSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        self.identities
            .lock()
            .unwrap()
            .push(request.identity.clone());
        let identity = request.identity.unwrap_or(WorkerIdentity::LocalSystem);
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
            session_id: request.session_id,
            pipe_name: "test-pipe".to_string(),
            identity,
        })
    }

    fn request_worker(
        &self,
        worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::StartDebugSession { .. }
                if worker.identity != WorkerIdentity::ActiveInteractiveUser =>
            {
                Ok(WorkerResponse::Failed {
                    code: "start_failed".to_string(),
                    message:
                        "native call failed with status 500: HRESULT 0x80070005: Access is denied."
                            .to_string(),
                    writes: Vec::new(),
                })
            }
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for FailingCreateWorkerSupervisor {
    fn create_worker(&self, _request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Err(ServiceError::Io(std::io::Error::from_raw_os_error(5)))
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        _request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        unreachable!("create_worker fails before worker requests")
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for FailingStartSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
            session_id: request.session_id,
            pipe_name: "test-pipe".to_string(),
            identity: WorkerIdentity::CurrentUserDevMode,
        })
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::StartDebugSession { .. } => Ok(WorkerResponse::Failed {
                code: "start_failed".to_string(),
                message: "native open failed".to_string(),
                writes: Vec::new(),
            }),
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for FailingEvalSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
            session_id: request.session_id,
            pipe_name: "test-pipe".to_string(),
            identity: WorkerIdentity::CurrentUserDevMode,
        })
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::EvalDebugCommand { .. } => Ok(WorkerResponse::Failed {
                code: "eval_failed".to_string(),
                message: "mock eval failed".to_string(),
                writes: Vec::new(),
            }),
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for BadEvalWriteSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(test_worker_handle(request.session_id))
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::EvalDebugCommand {
                session_id,
                operation_id: _,
                command,
                ..
            } => Ok(WorkerResponse::DebugCommand {
                result: DebugCommandResult {
                    session_id,
                    operation_id: None,
                    command,
                    output: "bad write".to_string(),
                    output_truncated: false,
                    full_output_byte_len: Some(9),
                    inline_output_byte_limit: Some(DEFAULT_INLINE_TEXT_BYTE_LIMIT as u64),
                    final_state: Some(DebugSessionState::Ready),
                    raw_output: None,
                    full_output_artifact_ref: None,
                    warnings: Vec::new(),
                    error: None,
                },
                writes: vec![WorkerArtifactWrite {
                    relative_path: PathBuf::from("outside.txt"),
                    kind: "debug.raw_output".to_string(),
                    byte_len: 9,
                    description: Some("invalid test write".to_string()),
                }],
            }),
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for FailingReverseOpenSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(test_worker_handle(request.session_id))
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::OpenReverseSession { .. } => Ok(WorkerResponse::Failed {
                code: "reverse_open_failed".to_string(),
                message: "open_database failed with result 4".to_string(),
                writes: Vec::new(),
            }),
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for FailingReverseCloseSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(test_worker_handle(request.session_id))
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::CloseReverseSession { .. } => Ok(WorkerResponse::Failed {
                code: "reverse_close_failed".to_string(),
                message: "mock IDA close failed".to_string(),
                writes: Vec::new(),
            }),
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl WorkerSupervisor for FailingReverseCoreSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(test_worker_handle(request.session_id))
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::ReverseCoreFunction { .. } => Ok(WorkerResponse::Failed {
                code: "reverse_core_failed".to_string(),
                message: "mock IDA core function failed".to_string(),
                writes: Vec::new(),
            }),
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }
}

fn test_worker_handle(session_id: SessionRef) -> WorkerHandle {
    WorkerHandle {
        worker_id: Id::new(format!("test-worker-{}", session_id.id.as_str())).unwrap(),
        session_id,
        pipe_name: "test-pipe".to_string(),
        identity: WorkerIdentity::CurrentUserDevMode,
    }
}

impl InstrumentedWorkerSupervisor {
    fn with_delay(delay_ms: u64) -> Self {
        Self {
            active: AtomicU64::new(0),
            max_active: AtomicU64::new(0),
            delay_ms,
            wait_for_cancel: false,
            canceled: AtomicBool::new(false),
            operation_id: Mutex::new(None),
        }
    }

    fn wait_for_cancel() -> Self {
        Self {
            wait_for_cancel: true,
            ..Self::with_delay(0)
        }
    }

    fn wait_for_operation_id(&self) -> Value {
        for _ in 0..100 {
            if let Some(operation_id) = self.operation_id.lock().unwrap().clone() {
                return serde_json::to_value(operation_id).unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("operation id was not captured");
    }

    fn enter_request(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        loop {
            let current = self.max_active.load(Ordering::SeqCst);
            if active <= current {
                break;
            }
            if self
                .max_active
                .compare_exchange(current, active, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
    }

    fn leave_request(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl WorkerSupervisor for InstrumentedWorkerSupervisor {
    fn create_worker(&self, request: WorkerCreateRequest) -> Result<WorkerHandle, ServiceError> {
        Ok(WorkerHandle {
            worker_id: Id::new(format!("test-worker-{}", request.session_id.id.as_str())).unwrap(),
            session_id: request.session_id.clone(),
            pipe_name: format!(r"\\.\pipe\test-{}", request.session_id.id.as_str()),
            identity: WorkerIdentity::CurrentUserDevMode,
        })
    }

    fn request_worker(
        &self,
        _worker: &WorkerHandle,
        request: WorkerRequest,
    ) -> Result<WorkerResponse, ServiceError> {
        match request {
            WorkerRequest::EvalDebugCommand {
                operation_id,
                session_id,
                command,
                artifact_dir,
            } => {
                *self.operation_id.lock().unwrap() = Some(operation_id.clone());
                self.enter_request();
                if self.wait_for_cancel {
                    for _ in 0..100 {
                        if self.canceled.load(Ordering::SeqCst) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(self.delay_ms));
                }
                let response =
                    write_mock_eval_response(session_id, operation_id, command, artifact_dir);
                self.leave_request();
                response
            }
            WorkerRequest::CloseSession { session_id } => {
                self.enter_request();
                std::thread::sleep(Duration::from_millis(self.delay_ms));
                self.leave_request();
                mock_worker_response(WorkerRequest::CloseSession { session_id })
            }
            other => mock_worker_response(other),
        }
    }

    fn cancel_worker_operation(
        &self,
        _worker: &WorkerHandle,
        _session_id: &SessionRef,
        _operation_id: &OperationRef,
    ) -> Result<WorkerCancelOutcome, ServiceError> {
        self.canceled.store(true, Ordering::SeqCst);
        Ok(WorkerCancelOutcome::Notified)
    }

    fn close_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        Ok(())
    }

    fn kill_worker(&self, _worker: &WorkerHandle) -> Result<(), ServiceError> {
        self.canceled.store(true, Ordering::SeqCst);
        Ok(())
    }
}
