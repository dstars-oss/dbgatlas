use super::*;

#[cfg(windows)]
#[test]
fn target_instance_exit_is_observed_without_a_backend_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let result = host
        .recording_start(Some(json!({
            "project_root": temp.path(), "target": {"type":"attach", "pid":42}
        })))
        .unwrap();
    let id: SessionRef = serde_json::from_value(result["session_id"].clone()).unwrap();
    let mut old_instance = crate::process_identity::inspect(std::process::id()).unwrap();
    // A different instance now occupies this PID: the recorded instance has ended.
    old_instance.creation_time_100ns = old_instance.creation_time_100ns.map(|time| time - 1);
    host.lock_state()
        .unwrap()
        .summaries
        .get_mut(id.id.as_str())
        .unwrap()
        .target
        .process = Some(old_instance);
    let summary = host.session_get(Some(json!({"session_id":id}))).unwrap();
    assert_eq!(summary["lifecycle"], "active");
    assert_eq!(summary["target_state"]["state"], "exited");
    assert_eq!(summary["target_state"]["source"], "process_monitor");
    host.recording_stop(Some(json!({"recording_id":id})))
        .unwrap();
}

#[test]
fn unknown_methods_do_not_create_worker_or_operation() {
    let host = ServiceHost::new(Arc::new(FailingCreateWorkerSupervisor));
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "unsupported.method".to_string(),
        params: Some(json!({})),
    });
    assert!(response.error.unwrap().message.contains("unknown method"));
    assert!(
        matches!(host.call_mcp_tool_output("unsupported.method", json!({})),
        Err(ServiceError::Rpc(message)) if message.contains("unknown DbgAtlas tool"))
    );
    assert!(host.lock_state().unwrap().sessions.is_empty());
    assert!(host.lock_state().unwrap().operations.is_empty());
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
        eval.result.unwrap()["result"]
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
    assert_eq!(result["result_delivery"]["truncated"], true);
    assert_eq!(
        result["result"].as_str().unwrap().len(),
        DEFAULT_INLINE_RESULT_BYTE_LIMIT
    );
    assert_eq!(
        result["result_delivery"]["full_byte_len"],
        json!(DEFAULT_INLINE_RESULT_BYTE_LIMIT + 100)
    );
    assert_eq!(
        result["result_delivery"]["full_artifact_ref"],
        result["raw_output_ref"]
    );
    assert!(result.get("output").is_none());
    assert!(result.get("output_truncated").is_none());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let raw_ref: ArtifactRef = serde_json::from_value(result["raw_output_ref"].clone()).unwrap();
    let raw = workspace.get_artifact(&raw_ref).unwrap().unwrap();
    let raw_path = workspace
        .resolve_artifact_relative_path(raw.relative_path)
        .unwrap();
    assert_eq!(
        fs::read_to_string(raw_path).unwrap().len(),
        DEFAULT_INLINE_RESULT_BYTE_LIMIT + 100
    );

    let close = close_request(&host, session_id);
    assert!(close.error.is_none(), "{:?}", close.error);
}

#[test]
fn debug_eval_recovers_complete_artifact_after_transport_failure() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(ArtifactThenTransportFailureSupervisor));
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let eval = eval_request(&host, session_id, ".echo recovery");
    assert!(eval.error.is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifact = workspace
        .list_artifacts()
        .unwrap()
        .into_iter()
        .find(|artifact| artifact.kind == "debug.raw_output")
        .expect("completed raw output is recovered");
    let path = workspace
        .resolve_artifact_relative_path(artifact.relative_path)
        .unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "complete output before transport failure"
    );
    assert!(!PathBuf::from(format!("{}.part", path.display())).exists());
}

#[test]
fn debug_eval_removes_partial_artifact_after_transport_failure() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(ArtifactThenTransportFailureSupervisor));
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let eval = eval_request(&host, session_id, ".echo partial-only");
    assert!(eval.error.is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    assert!(
        workspace
            .list_artifacts()
            .unwrap()
            .iter()
            .all(|artifact| artifact.kind != "debug.raw_output")
    );
    let raw_dir = workspace
        .root()
        .join("artifacts")
        .join("sessions")
        .read_dir()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("raw");
    assert!(raw_dir.read_dir().unwrap().next().is_none());
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
        result["steps"][0]["result"]
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

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["operation_status"], "success");
    assert!(
        result["steps"][0]["error"]
            .as_str()
            .unwrap()
            .contains("artifact path")
    );
    let state = host.lock_state().unwrap();
    assert!(state.operations.values().any(|operation| {
        operation.capability == "debug.eval_steps.step"
            && operation.status == ServiceOperationStatus::Success
            && operation.details.output_error.is_some()
    }));
    assert!(state.operations.values().any(|operation| {
        operation.capability == "debug.eval_steps"
            && operation.status == ServiceOperationStatus::Success
            && operation.details.output_error.is_some()
    }));
}

#[test]
fn debug_eval_artifact_registration_failure_finalizes_operation() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(BadEvalWriteSupervisor));
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();

    let response = eval_request(&host, session_id, ".echo invalid artifact");
    assert!(response.error.unwrap().message.contains("artifact path"));

    let state = host.lock_state().unwrap();
    assert!(state.operations.values().any(|operation| {
        operation.capability == "debug.eval"
            && operation.status == ServiceOperationStatus::Success
            && operation.details.output_error.is_some()
    }));
    drop(state);
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    assert!(
        workspace
            .list_operations()
            .unwrap()
            .iter()
            .any(|operation| {
                operation.capability == "debug.eval"
                    && matches!(operation.status, OperationStatus::Success)
            })
    );
    assert!(workspace.list_command_audit().unwrap().iter().any(|audit| {
        audit.capability == "debug.eval" && matches!(audit.status, OperationStatus::Success)
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
    let tools = mcp_tool_descriptors();
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
        recording["inputSchema"]["properties"]["target"]["oneOf"][0]["properties"]["start_directory"]
            ["type"],
        "string"
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["target"]["oneOf"][0]["additionalProperties"],
        false
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["target"]["oneOf"][1]["additionalProperties"],
        false
    );
    assert_eq!(
        recording["inputSchema"]["properties"]["target"]["oneOf"][2]["additionalProperties"],
        false
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
fn mcp_tool_input_schemas_are_top_level_objects() {
    let tools = mcp_tool_descriptors();

    for tool in tools {
        assert_eq!(
            tool["inputSchema"]["type"], "object",
            "tool `{}` must expose a top-level object inputSchema for strict MCP clients",
            tool["name"]
        );
    }
}

#[test]
fn repeated_close_reports_removed_session() {
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
    assert_eq!(second.error.unwrap().code, -32010);
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

    {
        let state = host.lock_state().unwrap();
        let session = state.sessions.values().next().unwrap();
        assert_eq!(session.state, DebugSessionState::Error);
        let operation = state
            .operations
            .values()
            .find(|operation| operation.capability == "debug.eval")
            .unwrap();
        assert_eq!(operation.status, ServiceOperationStatus::Unknown);
    }

    let cleanup = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "debug.session.kill".to_string(),
        params: Some(json!({ "session_id": session_id })),
    });
    assert!(cleanup.error.is_none(), "{:?}", cleanup.error);
    assert!(host.lock_state().unwrap().sessions.is_empty());
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
fn cancel_acceptance_does_not_override_successful_execution() {
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
    assert_eq!(get.result.unwrap()["status"], "success");
    assert_eq!(cancel.result.unwrap()["cancellation_requested"], true);
}

#[test]
fn busy_session_rejects_a_second_operation_without_queueing() {
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
    let responses = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(responses.iter().filter(|r| r.error.is_none()).count(), 1);
    assert_eq!(
        responses
            .iter()
            .filter(|r| r.error.as_ref().is_some_and(|e| e.code == -32017))
            .count(),
        1
    );

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
    assert!(host.lock_state().unwrap().sessions.is_empty());
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
    let missing_count = responses
        .iter()
        .filter(|response| {
            response
                .error
                .as_ref()
                .is_some_and(|error| matches!(error.code, -32010 | -32017))
        })
        .count();

    assert_eq!(success_count, 1);
    assert_eq!(missing_count, 1);
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
    assert!(matches!(
        eval.join().unwrap().error.unwrap().code,
        -32010 | -32017 | -32011
    ));
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
fn session_summary_binding_terminal_history_and_operation_context_are_stable() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let first = create_debug_session(&host, temp.path()).result.unwrap();
    let second = create_debug_session(&host, temp.path()).result.unwrap();
    let a: SessionRef = serde_json::from_value(first["session_id"].clone()).unwrap();
    let b: SessionRef = serde_json::from_value(second["session_id"].clone()).unwrap();
    let old = host.session_get(Some(json!({"session_id":a}))).unwrap();
    let other_worker = host.lock_state().unwrap().sessions[b.id.as_str()]
        .worker
        .clone();
    assert!(
        host.bind_session_worker(&a, &other_worker, "dbgeng")
            .is_err()
    );
    assert_ne!(
        old["worker"]["worker_id"],
        second["session"]["worker"]["worker_id"]
    );
    let eval = eval_request(&host, serde_json::to_value(&a).unwrap(), ".echo snapshot")
        .result
        .unwrap();
    let op: OperationRef = serde_json::from_value(eval["operation_id"].clone()).unwrap();
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let before = workspace.get_operation(&op).unwrap().unwrap();
    assert!(before.details.input.is_some());
    assert!(
        before
            .details
            .execution_context
            .as_ref()
            .unwrap()
            .worker
            .is_some()
    );
    close_request(&host, serde_json::to_value(&a).unwrap())
        .result
        .unwrap();
    let closed = host.session_get(Some(json!({"session_id":a}))).unwrap();
    assert_eq!(closed["lifecycle"], "closed");
    assert_eq!(closed["cleanup"]["complete"], true);
    assert_eq!(closed["worker"], old["worker"]);
    assert_eq!(
        workspace
            .get_operation(&op)
            .unwrap()
            .unwrap()
            .details
            .execution_context,
        before.details.execution_context
    );
    assert_eq!(host.session_list(None).unwrap()["count"], 1);
    assert_eq!(
        host.session_list(Some(json!({"include_terminal":true})))
            .unwrap()["count"],
        2
    );
    let history = workspace.list_operations().unwrap();
    assert_eq!(history.iter().filter(|r| r.operation_id == op).count(), 1);
    assert!(history.iter().any(|r| r.capability == "debug.session.close"
        && r.details.input.is_some()
        && r.details.update_seq >= 1));
}

#[test]
fn recording_alias_and_default_session_scope_are_compatible() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let result = host
        .recording_start(Some(
            json!({"project_root":temp.path(),"target":{"type":"attach","pid":42}}),
        ))
        .unwrap();
    assert_eq!(result["session_id"], result["recording_id"]);
    assert_eq!(host.session_list(None).unwrap()["count"], 0);
    assert_eq!(
        host.session_list(Some(json!({"kind":"recording"})))
            .unwrap()["count"],
        1
    );
    host.recording_kill(Some(json!({"recording_id":result["recording_id"]})))
        .unwrap();
    assert_eq!(
        host.session_list(Some(json!({"kind":"recording"})))
            .unwrap()["count"],
        0
    );
    assert_eq!(
        host.session_list(Some(json!({"kind":"recording","include_terminal":true})))
            .unwrap()["count"],
        1
    );
}
