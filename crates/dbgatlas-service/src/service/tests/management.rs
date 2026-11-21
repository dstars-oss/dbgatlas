use super::*;

fn rpc(host: &ServiceHost, method: &str, params: Value) -> Value {
    let reply = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: method.into(),
        params: Some(params),
    });
    assert!(reply.error.is_none(), "{:?}", reply.error);
    reply.result.unwrap()
}

#[test]
fn management_history_survives_host_restart_without_reviving_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let opened = rpc(
        &host,
        "session.open",
        json!({"project_root":temp.path(),"target":{"kind":"file","path":"fixture.dmp"},"wait_ms":30000}),
    );
    let sid = opened["session_id"].clone();
    let executed = rpc(
        &host,
        "session.exec",
        json!({"session_id":sid,"command":".echo management","wait_ms":30000}),
    );
    assert_eq!(executed["execution"]["status"], "success");
    let root = temp.path().join(INTERNAL_WORKSPACE_DIR);
    let workspace = test_history().open_workspace(&root).unwrap();
    let pending: OperationRecord = serde_json::from_value(json!({"operation_id":{"id":"pending-old"},"adapter_id":"dbgeng","capability":"session.exec","status":"running","summary":"incomplete","artifacts":[],"created_at":{"unix_millis":1},"session_id":sid})).unwrap();
    workspace.append_operation(&pending).unwrap();
    let mut known = ServiceOperation::success(
        pending.operation_id.clone(),
        "session.exec",
        Some(serde_json::from_value(sid.clone()).unwrap()),
        "completed but final journal append failed",
    );
    known.details.output_error = Some("simulated archive failure".into());
    host.lock_state()
        .unwrap()
        .operations
        .insert("pending-old".into(), known);
    let query = json!({"action":"workspace","project_root":temp.path()});
    let live = host.management_query(&query).unwrap();
    assert_eq!(live["sessions"][0]["current_service"], true);
    let known = live["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["operation_id"]["id"] == "pending-old")
        .unwrap();
    assert_eq!(known["status"], "success");
    assert_eq!(known["output_error"], "simulated archive failure");
    let restarted = ServiceHost::with_mock_workers();
    let history = restarted.management_query(&query).unwrap();
    assert_eq!(history["sessions"][0]["lifecycle"], "unknown");
    assert_eq!(history["sessions"][0]["current_service"], false);
    let filtered = restarted
        .management_query(
            &json!({"action":"workspace","project_root":temp.path(),"status":"unknown"}),
        )
        .unwrap();
    assert_eq!(filtered["filtered_count"], 1);
    assert_eq!(
        filtered["operations"][0]["operation_id"]["id"],
        "pending-old"
    );
    let artifact = &executed["execution"]["raw_output_ref"];
    let output = restarted
        .management_query(
            &json!({"action":"output","project_root":temp.path(),"artifact_id":artifact}),
        )
        .unwrap();
    assert!(output["text"].as_str().unwrap().contains("management"));
    let first = restarted
        .management_query(&json!({"action":"workspace","project_root":temp.path(),"limit":1}))
        .unwrap();
    let second = restarted
        .management_query(
            &json!({"action":"workspace","project_root":temp.path(),"limit":1,"offset":1}),
        )
        .unwrap();
    assert_eq!(first["has_more"], true);
    assert_ne!(
        first["operations"][0]["operation_id"],
        second["operations"][0]["operation_id"]
    );
    rpc(
        &host,
        "session.close",
        json!({"session_id":sid,"wait_ms":30000}),
    );
}

#[test]
fn management_output_is_bounded_and_preserves_utf8() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = test_history()
        .init_workspace(temp.path().join(INTERNAL_WORKSPACE_DIR), Default::default())
        .unwrap();
    let text = "界".repeat(30000);
    let relative = PathBuf::from("artifacts/output.txt");
    fs::write(workspace.root().join(&relative), &text).unwrap();
    let artifact = ArtifactMetadata {
        artifact_id: ArtifactRef::new(Id::new("utf8").unwrap()),
        kind: "text".into(),
        relative_path: relative,
        created_at: Timestamp::now(),
        operation_id: None,
        producer_session_id: None,
        producer_execution_id: None,
        byte_len: Some(text.len() as u64),
        description: None,
    };
    workspace.register_artifact(&artifact).unwrap();
    let host = ServiceHost::with_mock_workers();
    let first = host.management_query(&json!({"action":"output","project_root":temp.path(),"artifact_id":artifact.artifact_id})).unwrap();
    assert_eq!(first["lossy"], false);
    assert_eq!(first["has_more"], true);
    assert!(first["next_offset"].as_u64().unwrap() <= 65536);
    let second = host.management_query(&json!({"action":"output","project_root":temp.path(),"artifact_id":artifact.artifact_id,"offset":first["next_offset"]})).unwrap();
    assert_eq!(
        format!(
            "{}{}",
            first["text"].as_str().unwrap(),
            second["text"].as_str().unwrap()
        ),
        text
    );
    assert!(
        host.management_query(
            &json!({"action":"output","project_root":temp.path(),"artifact_id":"missing"})
        )
        .is_err()
    );
    fs::remove_file(workspace.root().join("artifacts/output.txt")).unwrap();
    assert!(host.management_query(&json!({"action":"output","project_root":temp.path(),"artifact_id":artifact.artifact_id})).is_err());
}

#[test]
fn management_listener_is_separate_read_only_and_same_origin() {
    let endpoint = unused_loopback_endpoint();
    let host = ServiceHost::with_mock_workers().with_management_bind(Some(endpoint));
    let server = start_mock_http_service_with_host(host);
    let response = post_json(
        endpoint,
        "/api/query",
        Some(&format!("http://{endpoint}")),
        json!({"action":"overview"}),
    );
    assert_eq!(http_status(&response), 200);
    assert_eq!(
        http_body_json(&response)["version"],
        env!("CARGO_PKG_VERSION")
    );
    for origin in ["http://evil.example", "http://localhost:1", "null"] {
        assert_eq!(
            http_status(&post_json(
                endpoint,
                "/api/query",
                Some(origin),
                json!({"action":"overview"})
            )),
            403
        );
    }
    assert_eq!(
        http_status(&post_json(
            endpoint,
            "/rpc",
            None,
            json!({"method":"session.kill"})
        )),
        404
    );
    assert_eq!(
        http_status(&post_json(
            endpoint,
            "/api/query",
            None,
            json!({"action":"session.kill"})
        )),
        400
    );
    assert_ne!(
        http_status(&post_json(
            server.endpoint,
            "/api/query",
            None,
            json!({"action":"overview"})
        )),
        200
    );
    let mut stream = TcpStream::connect(endpoint).unwrap();
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut html = String::new();
    stream.read_to_string(&mut html).unwrap();
    assert!(html.contains("Content-Security-Policy:"));
    assert!(html.contains("调试记录"));
    let mut stream = TcpStream::connect(endpoint).unwrap();
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: malicious.example:{}\r\nConnection: close\r\n\r\n",
        endpoint.port()
    )
    .unwrap();
    let mut denied = String::new();
    stream.read_to_string(&mut denied).unwrap();
    assert_eq!(http_status(&denied), 403);
    server.stop();
    assert!(TcpStream::connect(endpoint).is_err());
}

#[test]
fn management_refuses_public_bind_and_does_not_create_workspaces() {
    let result = run_http_service_until(
        ServiceConfig {
            bind: unused_loopback_endpoint(),
        },
        ServiceHost::with_mock_workers()
            .with_management_bind(Some("0.0.0.0:7332".parse().unwrap())),
        ServiceShutdown::new(),
    );
    assert!(matches!(result, Err(ServiceError::NonLoopbackBind(_))));
    let temp = tempfile::tempdir().unwrap();
    assert!(
        ServiceHost::with_mock_workers()
            .management_query(&json!({"action":"workspace","project_root":temp.path()}))
            .is_err()
    );
    assert!(!temp.path().join(INTERNAL_WORKSPACE_DIR).exists());
}

#[test]
fn central_history_survives_restart_offline_files_and_shared_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("central.sqlite3");
    let mut host = ServiceHost::with_mock_workers();
    host.history = HistoryStore::new(database.clone());
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let open = |host: &ServiceHost, root: &Path| {
        rpc(
            host,
            "session.open",
            json!({"project_root":root,"target":{"kind":"file","path":"fixture.dmp"},"wait_ms":30000}),
        )
    };
    let s1 = open(&host, &first)["session_id"].clone();
    let s2 = open(&host, &second)["session_id"].clone();
    let marker = rpc(
        &host,
        "session.exec",
        json!({"session_id":s1,"command":".echo central-one","wait_ms":30000}),
    );
    let id = marker["execution_id"]["id"].as_str().unwrap();
    let artifact = &marker["execution"]["raw_output_ref"];
    let consumer = rpc(
        &host,
        "session.open",
        json!({"project_root":first,"target":{"kind":"file","artifact_ref":artifact},"wait_ms":30000}),
    );
    for sid in [&s1, &s2, &consumer["session_id"]] {
        rpc(
            &host,
            "session.close",
            json!({"session_id":sid,"wait_ms":30000}),
        );
    }
    drop(host);
    let mut host = ServiceHost::with_mock_workers();
    host.history = HistoryStore::new(database);
    assert_eq!(
        rpc(&host, "history.query", json!({}))["projects"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let provenance = rpc(
        &host,
        "history.query",
        json!({"action":"provenance","project_root":first,"artifact_id":artifact}),
    );
    assert_eq!(
        provenance["artifact"]["producer_execution_id"],
        marker["execution_id"]
    );
    assert_eq!(
        provenance["consumer_sessions"],
        json!([consumer["session_id"]])
    );
    let changes = rpc(
        &host,
        "history.query",
        json!({"action":"history","project_root":first,"execution_id":id}),
    );
    assert!(changes["entries"].as_array().unwrap().len() >= 2);
    let time = marker["execution"]["created_at"]["unix_millis"]
        .as_u64()
        .unwrap();
    let filter = json!({"action":"workspace","project_root":first,"search":"central-one","start_ms":time,"end_ms":time+1});
    assert_eq!(
        rpc(&host, "history.query", filter.clone())["filtered_count"],
        1
    );
    assert_eq!(
        rpc(
            &host,
            "history.query",
            json!({"action":"workspace","project_root":second,"search":id})
        )["filtered_count"],
        0
    );
    let moved = temp.path().join("moved");
    fs::rename(&first, &moved).unwrap();
    assert_eq!(
        rpc(&host, "history.query", filter)["operations"][0]["status"],
        "success"
    );
    assert!(
        host.management_query(
            &json!({"action":"output","project_root":first,"artifact_id":artifact})
        )
        .is_err()
    );
    assert!(!first.exists());
    host.history
        .relink(&moved.join(INTERNAL_WORKSPACE_DIR), &host.service_instance)
        .unwrap();
    assert!(
        rpc(
            &host,
            "history.query",
            json!({"action":"output","project_root":moved,"artifact_id":artifact})
        )["text"]
            .as_str()
            .unwrap()
            .contains("central-one")
    );
}

#[test]
fn moving_active_workspace_preserves_control_and_requires_close_before_relink() {
    let temp = tempfile::tempdir().unwrap();
    let mut host = ServiceHost::with_mock_workers();
    host.history = HistoryStore::new(temp.path().join("history.sqlite3"));
    let root = temp.path().join("project");
    let moved = temp.path().join("moved");
    let opened = rpc(
        &host,
        "session.open",
        json!({"project_root":root,"target":{"kind":"file","path":"test.dmp"},"wait_ms":30000}),
    );
    fs::rename(&root, &moved).unwrap();
    let path = moved.join(INTERNAL_WORKSPACE_DIR);
    assert!(
        host.workspace_request("workspace.relink", json!({"path":path}))
            .is_err()
    );
    let close = rpc(
        &host,
        "session.close",
        json!({"session_id":opened["session_id"],"wait_ms":30000}),
    );
    assert_eq!(close["execution"]["status"], "success");
    assert!(close["execution"]["output_error"].is_string());
    assert!(!root.exists());
    let workspace = host
        .history
        .lookup(root.join(INTERNAL_WORKSPACE_DIR))
        .unwrap();
    let mut saved = workspace.sessions().unwrap().pop().unwrap();
    saved.lifecycle = dbgatlas_model::SessionLifecycle::Failed;
    saved.cleanup.complete = false;
    workspace.save_session(&saved).unwrap();
    assert!(
        host.workspace_request("workspace.relink", json!({"path":path}))
            .is_err()
    );
    saved.cleanup.complete = true;
    workspace.save_session(&saved).unwrap();
    host.workspace_request("workspace.relink", json!({"path":path}))
        .unwrap();
    let facts = rpc(
        &host,
        "history.query",
        json!({"action":"workspace","project_root":moved,"search":close["execution_id"]["id"]}),
    );
    assert_eq!(facts["operations"][0]["status"], "success");
    assert!(facts["operations"][0]["output_error"].is_string());
}
