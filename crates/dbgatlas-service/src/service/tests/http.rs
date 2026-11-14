use super::*;

#[test]
fn validates_loopback_service_bind() {
    let config = ServiceConfig {
        bind: "0.0.0.0:7331".parse().unwrap(),
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
fn http_handler_waits_for_a_delayed_request_body() {
    let server = start_mock_http_service();
    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    }))
    .unwrap();
    let mut stream = TcpStream::connect(server.endpoint).unwrap();
    write!(
        stream,
        "POST /mcp HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        server.endpoint,
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();

    // Give the accept loop time to hand the partially received request to a
    // handler. The handler must block for the remaining body bytes.
    std::thread::sleep(Duration::from_millis(150));
    stream.write_all(body.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert_eq!(http_status(&response), 200);
    assert_eq!(
        http_body_json(&response)["result"]["serverInfo"]["name"],
        "dbgatlas-mcp"
    );
    server.stop();
}

#[test]
fn http_mcp_tools_list_returns_current_tools() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
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
        "recording.start",
        "recording.ttd",
        "recording.status",
        "recording.stop",
        "recording.cancel",
        "recording.kill",
        "artifact.list",
        "operation.list",
        "audit.list_commands",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == tool_name),
            "{tool_name} tool is listed"
        );
    }
    assert!(!tools.iter().any(|tool| tool["name"] == "workspace.facts"));
    server.stop();
}

#[test]
fn http_mcp_ping_returns_empty_result() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
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
    assert_eq!(eval["result_delivery"]["truncated"], false);
    assert_eq!(
        eval["result_delivery"]["full_artifact_ref"],
        eval["raw_output_ref"]
    );
    assert!(eval.get("output").is_none());
    server.stop();
}

#[test]
fn http_mcp_session_list_returns_debug_sessions() {
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
    let list = mcp_tool_call(server.endpoint, "session.list", json!({}));
    let sessions = list["sessions"].as_array().unwrap();
    let debug_session = sessions
        .iter()
        .find(|session| session["session_id"] == debug["session_id"])
        .expect("debug session is listed");
    assert_eq!(list["count"], 1);
    assert_eq!(debug_session["capability"], "debug");
    assert_eq!(debug_session["state"], "Ready");
    assert_eq!(debug_session["close_tool"], "debug.session.close");
    assert_eq!(debug_session["kill_tool"], "debug.session.kill");
    assert!(debug_session["target"].is_object());
    server.stop();
}

#[test]
fn http_mcp_project_fact_lists_read_persisted_layers() {
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

    let commands = mcp_tool_call(
        server.endpoint,
        "audit.list_commands",
        json!({ "project_root": temp.path() }),
    );
    let operations = mcp_tool_call(
        server.endpoint,
        "operation.list",
        json!({ "project_root": temp.path() }),
    );
    let artifacts = mcp_tool_call(
        server.endpoint,
        "artifact.list",
        json!({ "project_root": temp.path() }),
    );

    assert_eq!(commands["commands"].as_array().unwrap().len(), 1);
    assert!(
        operations["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation["capability"] == "debug.eval")
    );
    assert!(!artifacts["artifacts"].as_array().unwrap().is_empty());

    let removed = post_json(
        server.endpoint,
        "/mcp",
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": "workspace.facts",
                "arguments": { "path": temp.path().join(INTERNAL_WORKSPACE_DIR) }
            }
        }),
    );
    let removed = http_body_json(&removed);
    assert!(
        removed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown DbgAtlas tool `workspace.facts`")
    );
    server.stop();
}

#[test]
fn project_fact_lists_require_project_root_and_reject_internal_workspace_path() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();

    let missing = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "artifact.list".to_string(),
        params: Some(json!({ "project_root": temp.path() })),
    });
    assert!(missing.error.is_some());

    let create = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "debug.session.create".to_string(),
        params: Some(json!({
            "project_root": temp.path(),
            "target": { "kind": "file", "path": "sample.dmp" }
        })),
    });
    assert!(create.error.is_none(), "{:?}", create.error);

    let internal_path = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "artifact.list".to_string(),
        params: Some(json!({
            "project_root": temp.path().join(INTERNAL_WORKSPACE_DIR)
        })),
    });
    assert!(internal_path.error.is_some());

    let artifacts = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "artifact.list".to_string(),
        params: Some(json!({ "project_root": temp.path() })),
    });
    assert!(artifacts.error.is_none(), "{:?}", artifacts.error);
    assert!(artifacts.result.unwrap()["artifacts"].is_array());
}

#[test]
fn project_fact_lists_cover_empty_fallback_and_missing_projects() {
    let empty = tempfile::tempdir().unwrap();
    Workspace::init(
        empty.path().join(INTERNAL_WORKSPACE_DIR),
        Default::default(),
    )
    .unwrap();
    let fallback = tempfile::tempdir().unwrap();
    let trace = fallback
        .path()
        .join(INTERNAL_WORKSPACE_DIR)
        .join("artifacts/recordings/recording-1/traces/sample.run");
    fs::create_dir_all(trace.parent().unwrap()).unwrap();
    fs::write(&trace, b"run").unwrap();
    let missing = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();

    for (method, field) in [
        ("artifact.list", "artifacts"),
        ("operation.list", "operations"),
        ("audit.list_commands", "commands"),
    ] {
        let empty_result = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params: Some(json!({ "project_root": empty.path() })),
        });
        assert!(
            empty_result.error.is_none(),
            "{method}: {:?}",
            empty_result.error
        );
        assert!(
            empty_result.result.unwrap()[field]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let fallback_result = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: method.to_string(),
            params: Some(json!({ "project_root": fallback.path() })),
        });
        assert!(
            fallback_result.error.is_none(),
            "{method}: {:?}",
            fallback_result.error
        );
        let rows = fallback_result.result.unwrap()[field]
            .as_array()
            .unwrap()
            .clone();
        if method == "artifact.list" {
            assert_eq!(rows.len(), 1);
        } else {
            assert!(rows.is_empty());
        }

        let missing_result = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(3)),
            method: method.to_string(),
            params: Some(json!({ "project_root": missing.path() })),
        });
        assert!(missing_result.error.is_some(), "{method} should fail");
    }
}

#[test]
fn http_mcp_recording_lifecycle_matches_rpc_behavior() {
    let temp = tempfile::tempdir().unwrap();
    let server = start_mock_http_service();
    let started = mcp_tool_call(
        server.endpoint,
        "recording.start",
        json!({
            "project_root": temp.path(),
            "target": { "type": "attach", "pid": 42 }
        }),
    );
    let recording_id = started["recording_id"].clone();
    assert_eq!(started["state"], "running");

    let status = mcp_tool_call(
        server.endpoint,
        "recording.status",
        json!({ "recording_id": recording_id.clone() }),
    );
    assert_eq!(status["state"], "running");

    let stopped = mcp_tool_call(
        server.endpoint,
        "recording.stop",
        json!({ "recording_id": recording_id }),
    );
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(stopped["operation_status"], "success");
    server.stop();
}

#[test]
fn http_mcp_recording_cancel_and_kill_match_rpc_shapes() {
    for (method, expected_state, expected_status) in [
        ("recording.cancel", "canceled", "canceled"),
        ("recording.kill", "killed", "failed"),
    ] {
        let mcp_temp = tempfile::tempdir().unwrap();
        let server = start_mock_http_service();
        let started = mcp_tool_call(
            server.endpoint,
            "recording.start",
            json!({
                "project_root": mcp_temp.path(),
                "target": { "type": "attach", "pid": 42 }
            }),
        );
        let mcp_result = mcp_tool_call(
            server.endpoint,
            method,
            json!({ "recording_id": started["recording_id"].clone() }),
        );

        let rpc_temp = tempfile::tempdir().unwrap();
        let host = ServiceHost::with_mock_workers();
        let started = create_recording(
            &host,
            rpc_temp.path(),
            json!({ "type": "attach", "pid": 42 }),
        );
        let rpc_result = host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(2)),
            method: method.to_string(),
            params: Some(
                json!({ "recording_id": started.result.unwrap()["recording_id"].clone() }),
            ),
        });
        assert!(
            rpc_result.error.is_none(),
            "{method}: {:?}",
            rpc_result.error
        );
        let rpc_result = rpc_result.result.unwrap();

        assert_eq!(mcp_result["state"], expected_state);
        assert_eq!(mcp_result["operation_status"], expected_status);
        assert_eq!(mcp_result["state"], rpc_result["state"]);
        assert_eq!(
            mcp_result["operation_status"],
            rpc_result["operation_status"]
        );
        assert_eq!(
            mcp_result["artifact_refs"].as_array().unwrap().len(),
            rpc_result["artifact_refs"].as_array().unwrap().len()
        );
        server.stop();
    }
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
fn http_mcp_accepts_requests_without_authentication() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/mcp",
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    );

    assert_eq!(http_status(&response), 200);
    assert!(http_body_json(&response)["result"]["tools"].is_array());
    server.stop();
}

#[test]
fn http_endpoints_reject_invalid_origins_without_authentication() {
    let server = start_mock_http_service();
    for (path, method) in [("/mcp", "tools/list"), ("/rpc", "service.health")] {
        for origin in ["http://localhost.evil.test", "https://example.com", "null"] {
            let response = post_json(
                server.endpoint,
                path,
                Some(origin),
                json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}}),
            );
            assert_eq!(http_status(&response), 403, "{path} {origin}");
            assert_eq!(http_body_json(&response)["error"]["code"], -32002);
        }
    }
    server.stop();
}

#[test]
fn http_rpc_path_still_returns_health() {
    let server = start_mock_http_service();
    let response = post_json(
        server.endpoint,
        "/rpc",
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

#[test]
fn service_shutdown_closes_debug_workers() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(ShutdownRecordingSupervisor::new());
    let host = ServiceHost::new(supervisor.clone());
    let debug = create_debug_session(&host, temp.path());
    assert!(debug.error.is_none(), "{:?}", debug.error);

    host.shutdown_workers_gracefully(Duration::from_millis(200));

    let requests = supervisor.requests.lock().unwrap();
    assert!(requests.contains(&"close_session"));
    assert_eq!(supervisor.close_count.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.kill_count.load(Ordering::SeqCst), 0);
}

#[test]
fn service_shutdown_kills_worker_when_request_lock_is_busy() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(ShutdownRecordingSupervisor::new());
    let host = ServiceHost::new(supervisor.clone());
    let session_id = create_debug_session(&host, temp.path()).result.unwrap()["session_id"].clone();
    let session_id = session_id["id"].as_str().unwrap().to_string();
    let request_lock = {
        let state = host.lock_state().unwrap();
        state
            .sessions
            .get(&session_id)
            .unwrap()
            .request_lock
            .clone()
    };
    let _guard = request_lock.lock().unwrap();

    host.shutdown_workers_gracefully(Duration::from_millis(20));

    assert_eq!(supervisor.close_count.load(Ordering::SeqCst), 0);
    assert_eq!(supervisor.kill_count.load(Ordering::SeqCst), 1);
}
