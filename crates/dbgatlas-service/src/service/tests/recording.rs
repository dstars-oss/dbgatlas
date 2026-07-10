use super::*;

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
fn recording_ttd_launch_uses_start_directory() {
    let temp = tempfile::tempdir().unwrap();
    let start_directory = temp.path().join("launch-cwd");
    fs::create_dir_all(&start_directory).unwrap();
    let (host, invocations) = ttd_test_host_and_invocations(MockTtdRunnerMode::CompletedWithTrace);
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.ttd".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": {
                "kind": "launch",
                "executable": "app.exe",
                "start_directory": start_directory.clone(),
                "args": ["--sample"]
            },
            "timeout_ms": 1000
        })),
    });

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().unwrap();
    assert_eq!(result["mode"], "launch");
    let invocations = invocations.lock().unwrap();
    assert_eq!(invocations.len(), 1);
    assert_eq!(
        invocations[0].current_dir.as_deref(),
        Some(start_directory.as_path())
    );
    let args = invocations[0]
        .args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(args.contains(&"-launch".to_string()));
    assert!(args.contains(&"app.exe".to_string()));
    assert!(!args.contains(&start_directory.to_string_lossy().into_owned()));
    drop(invocations);

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
    assert_eq!(metadata["target"]["kind"], "launch");
    assert_eq!(
        metadata["target"]["start_directory"].as_str().unwrap(),
        start_directory.to_string_lossy().as_ref()
    );
}

#[test]
fn recording_ttd_launch_rejects_missing_start_directory() {
    let temp = tempfile::tempdir().unwrap();
    let start_directory = temp.path().join("missing");
    let (host, invocations) = ttd_test_host_and_invocations(MockTtdRunnerMode::CompletedWithTrace);
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.ttd".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": {
                "kind": "launch",
                "executable": "app.exe",
                "start_directory": start_directory.clone()
            },
            "timeout_ms": 1000
        })),
    });

    let error = response.error.as_ref().unwrap();
    assert!(
        error
            .message
            .contains("start_directory does not exist or is not a directory")
    );
    assert!(invocations.lock().unwrap().is_empty());
}

#[test]
fn recording_ttd_rejects_start_directory_for_attach() {
    let temp = tempfile::tempdir().unwrap();
    let (host, invocations) = ttd_test_host_and_invocations(MockTtdRunnerMode::CompletedWithTrace);
    let response = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "recording.ttd".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": {
                "kind": "attach",
                "pid": 4242,
                "start_directory": temp.path()
            },
            "timeout_ms": 1000
        })),
    });

    let error = response.error.as_ref().unwrap();
    assert!(error.message.contains("unknown field"));
    assert!(invocations.lock().unwrap().is_empty());
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
        None,
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
fn ttd_active_user_helper_args_include_current_dir_before_separator() {
    let args = vec![
        OsString::from("-out"),
        OsString::from(r"C:\case\traces"),
        OsString::from("-launch"),
        OsString::from(r"C:\tools\app.exe"),
    ];
    let helper_args = ttd_command_helper_args(
        Path::new(r"C:\ttd\TTD.exe"),
        &args,
        Some(Path::new(r"C:\work")),
        Path::new(r"C:\case\recorder.stdout.txt"),
        Path::new(r"C:\case\recorder.stderr.txt"),
    );
    let text = helper_args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    let current_dir = text.iter().position(|arg| arg == "--current-dir").unwrap();
    let separator = text.iter().position(|arg| arg == "--").unwrap();
    assert!(current_dir < separator);
    assert_eq!(text[current_dir + 1], r"C:\work");
    assert_eq!(
        &text[separator + 1..],
        ["-out", r"C:\case\traces", "-launch", r"C:\tools\app.exe"]
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
