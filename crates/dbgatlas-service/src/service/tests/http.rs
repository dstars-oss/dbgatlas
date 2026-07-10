use super::*;

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
        "reverse.py_eval",
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
fn http_mcp_py_eval_is_available_without_opt_in() {
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
        "reverse.py_eval",
        json!({
            "session_id": open["session_id"],
            "code": "print('runs without an opt-in')"
        }),
    );

    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["function"], "py_eval");
    assert!(result.get("result").is_some());
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
fn http_mcp_reverse_decompile_accepts_module_rva_without_new_tool() {
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
        "reverse.decompile",
        json!({
            "session_id": open["session_id"],
            "addr": "libxsse+0x93660"
        }),
    );

    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["function"], "decompile");
    assert_eq!(result["result"]["addr"], 0x140093660u64);
    assert_eq!(result["result"]["normalized_address"]["kind"], "module_rva");
    assert_eq!(result["normalized_addresses"][0]["module"], "libxsse");
    assert_eq!(result["address_warnings"].as_array().unwrap().len(), 0);
    server.stop();
}

#[test]
fn http_mcp_reverse_decompile_accepts_windbg_backtick_hex() {
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
        "reverse.decompile",
        json!({
            "session_id": open["session_id"],
            "addr": "00000001`40093660"
        }),
    );

    assert_eq!(result["operation_status"], "success");
    assert_eq!(result["result"]["addr"], 0x140093660u64);
    assert_eq!(result["result"]["normalized_address"]["kind"], "ida_ea");
    server.stop();
}

#[test]
fn http_mcp_reverse_lookup_funcs_rejects_ttd_position() {
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
    let body = mcp_tool_call_body(
        server.endpoint,
        "reverse.lookup_funcs",
        json!({
            "session_id": open["session_id"],
            "queries": ["151E:2020"]
        }),
    );

    assert_eq!(body["result"]["isError"], true, "{body}");
    let text: Value =
        serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        text["error"]["message"]
            .as_str()
            .unwrap()
            .contains("TTD position")
    );
    server.stop();
}

#[test]
fn http_mcp_reverse_write_tool_returns_normalized_address_summary() {
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
        "reverse.rename",
        json!({
            "session_id": open["session_id"],
            "items": [{
                "kind": "function",
                "addr": "libxsse+0x93660",
                "new_name": "Detect_FormatCandidates_0x93660"
            }]
        }),
    );

    assert_eq!(result["operation_status"], "success");
    assert_eq!(
        result["result"]["items"][0]["normalized_address"]["kind"],
        "module_rva"
    );
    assert_eq!(result["normalized_addresses"][0]["rva"], 0x93660u64);
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
        Some("test-token"),
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
fn http_mcp_accepts_case_insensitive_bearer_scheme() {
    let server = start_mock_http_service();
    let response = post_json_with_authorization_header(
        server.endpoint,
        "/mcp",
        Some("bearer test-token"),
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
fn http_mcp_accepts_extra_bearer_header_spacing() {
    let server = start_mock_http_service();
    let response = post_json_with_authorization_header(
        server.endpoint,
        "/mcp",
        Some("Bearer    test-token"),
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
fn http_mcp_rejects_bearer_header_with_extra_parts() {
    let server = start_mock_http_service();
    let response = post_json_with_authorization_header(
        server.endpoint,
        "/mcp",
        Some("Bearer test-token extra"),
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

#[test]
fn service_shutdown_closes_debug_and_reverse_workers() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(ShutdownRecordingSupervisor::new());
    let host = ServiceHost::new(supervisor.clone());
    let debug = create_debug_session(&host, temp.path());
    assert!(debug.error.is_none(), "{:?}", debug.error);
    let _reverse_session_id = open_reverse_session(&host, temp.path());

    host.shutdown_workers_gracefully(Duration::from_millis(200));

    let requests = supervisor.requests.lock().unwrap();
    assert!(requests.contains(&"close_session"));
    assert!(requests.contains(&"close_reverse_session"));
    assert_eq!(supervisor.close_count.load(Ordering::SeqCst), 2);
    assert_eq!(supervisor.kill_count.load(Ordering::SeqCst), 0);
}

#[test]
fn service_shutdown_kills_worker_when_request_lock_is_busy() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(ShutdownRecordingSupervisor::new());
    let host = ServiceHost::new(supervisor.clone());
    let session_id = open_reverse_session(&host, temp.path());
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
