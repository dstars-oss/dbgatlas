use super::*;

#[test]
fn default_service_root_uses_program_files_dbgatlas() {
    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));

    assert_eq!(
        default_windows_service_root(),
        program_files.join("DbgAtlas")
    );
}

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
