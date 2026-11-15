use super::*;

fn http(endpoint: SocketAddr, method: &str, version: Option<&str>, body: Value) -> String {
    let body = serde_json::to_string(&body).unwrap();
    let header = version
        .map(|v| format!("MCP-Protocol-Version: {v}\r\n"))
        .unwrap_or_default();
    let mut stream = TcpStream::connect(endpoint).unwrap();
    write!(stream,"{method} /mcp HTTP/1.1\r\nHost: {endpoint}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n{header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}
fn tool(endpoint: SocketAddr, version: &str, name: &str, args: Value) -> Value {
    http_body_json(&http(
        endpoint,
        "POST",
        Some(version),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}}),
    ))
}
#[test]
fn negotiation_headers_and_response_shapes_do_not_leak_between_clients() {
    let server = start_mock_http_service();
    for (requested, expected) in [
        ("2024-11-05", "2024-11-05"),
        ("2025-03-26", "2025-03-26"),
        ("2025-06-18", "2025-06-18"),
        ("future", "2025-06-18"),
    ] {
        let result = http_body_json(&http(
            server.endpoint,
            "POST",
            None,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":requested,"capabilities":{"tasks":{}}}}),
        ));
        assert_eq!(result["result"]["protocolVersion"], expected);
        assert!(result["result"]["capabilities"].get("tasks").is_none());
    }
    for version in ["2025-06-18", "2024-11-05", "2025-06-18", "2025-03-26"] {
        let result = tool(server.endpoint, version, "service.health", json!({}));
        assert_eq!(
            result["result"].get("structuredContent").is_some(),
            version == "2025-06-18"
        );
        if version == "2025-06-18" {
            assert_eq!(result["result"]["structuredContent"]["status"], "ok");
        } else {
            let text: Value =
                serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(text["status"], "ok");
        }
    }
    let invalid = http(
        server.endpoint,
        "POST",
        Some("2099-01-01"),
        json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
    );
    assert_eq!(http_status(&invalid), 400);
    assert_eq!(
        http_status(&http(server.endpoint, "GET", None, Value::Null)),
        405
    );
    assert_eq!(
        http_status(&http(server.endpoint, "DELETE", None, Value::Null)),
        405
    );
    assert_eq!(
        http_status(&http(
            server.endpoint,
            "POST",
            None,
            json!({"jsonrpc":"2.0","id":1,"result":{}})
        )),
        202
    );
    server.stop();
}
#[test]
fn modern_result_has_one_body_and_observation_does_not_report_business_failure() {
    let temp = tempfile::tempdir().unwrap();
    let server =
        start_mock_http_service_with_host(ServiceHost::new(Arc::new(FailingEvalSupervisor)));
    let open = tool(
        server.endpoint,
        "2025-06-18",
        "session.open",
        json!({"project_root":temp.path(),"target":{"kind":"file","path":"fixture.dmp"}}),
    );
    let sid = &open["result"]["structuredContent"]["session_id"];
    let result = tool(
        server.endpoint,
        "2025-06-18",
        "session.exec",
        json!({"session_id":sid,"command":".echo unique-native-command"}),
    );
    assert_eq!(result["result"]["isError"], true);
    let structured = &result["result"]["structuredContent"];
    assert_eq!(structured["execution"]["status"], "failed");
    assert!(
        !result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unique-native-command")
    );
    let observed = tool(
        server.endpoint,
        "2025-06-18",
        "execution.get",
        json!({"execution_id":structured["execution_id"]}),
    );
    assert_eq!(observed["result"]["isError"], false);
    assert_eq!(observed["result"]["structuredContent"]["status"], "failed");
    assert_eq!(
        tool(server.endpoint, "2025-06-18", "missing.tool", json!({}))["error"]["code"],
        -32602
    );
    server.stop();
}
#[test]
fn notifications_cannot_dispatch_tools_or_cancel_business_execution() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(InstrumentedWorkerSupervisor::with_delay(200)));
    let server = start_mock_http_service_with_host(host.clone());
    let notification = http(
        server.endpoint,
        "POST",
        Some("2025-06-18"),
        json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"session.open","arguments":{"project_root":temp.path(),"target":{"kind":"file","path":"fixture.dmp"}}}}),
    );
    assert_eq!(http_status(&notification), 202);
    assert!(host.lock_state().unwrap().summaries.is_empty());
    let open = tool(
        server.endpoint,
        "2025-06-18",
        "session.open",
        json!({"project_root":temp.path(),"target":{"kind":"file","path":"fixture.dmp"}}),
    );
    let running = tool(
        server.endpoint,
        "2025-06-18",
        "session.exec",
        json!({"session_id":open["result"]["structuredContent"]["session_id"],"command":".echo finish","wait_ms":0}),
    );
    let id = &running["result"]["structuredContent"]["execution_id"];
    let canceled = http(
        server.endpoint,
        "POST",
        Some("2025-06-18"),
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}),
    );
    assert_eq!(http_status(&canceled), 202);
    let completed = tool(
        server.endpoint,
        "2025-06-18",
        "execution.get",
        json!({"execution_id":id,"wait_ms":30000}),
    );
    assert_eq!(
        completed["result"]["structuredContent"]["status"],
        "success"
    );
    server.stop();
}
#[test]
fn canonical_inventory_preserves_binary_and_recording_tools_and_rpc_aliases() {
    let tools = mcp_tool_descriptors();
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        assert!(!name.starts_with("operation."));
        assert!(!name.starts_with("debug.") || name == "debug.read_memory");
    }
    for name in [
        "session.open",
        "session.exec",
        "session.interrupt",
        "session.help",
        "execution.get",
        "execution.read",
        "execution.cancel",
        "execution.list",
        "debug.read_memory",
        "recording.ttd",
        "service.update",
        "artifact.list",
        "audit.list_commands",
    ] {
        assert!(tools.iter().any(|t| t["name"] == name), "missing {name}");
    }
    assert!(method_spec("debug.eval").is_some());
    assert!(method_spec("operation.get").is_some());
}
