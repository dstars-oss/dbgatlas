use super::*;

fn call(host: &ServiceHost, method: &str, params: Value) -> Result<Value, JsonRpcError> {
    let reply = host.handle_rpc(JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: method.into(),
        params: Some(params),
    });
    match reply.error {
        Some(error) => Err(error),
        None => Ok(reply.result.unwrap()),
    }
}

fn open(host: &ServiceHost, root: &Path) -> Value {
    call(
        host,
        "session.open",
        json!({"project_root":root,"target":{"kind":"file","path":"fixture.dmp"},"wait_ms":30000}),
    )
    .unwrap()
}

fn complete(host: &ServiceHost, id: &Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut value = call(host, "execution.get", json!({"execution_id":id})).unwrap();
    loop {
        if value["output_complete"] == true
            || (value["status"] != "running"
                && value["status"] != "pending"
                && !value["output_error"].is_null())
        {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "execution did not finish: {value}"
        );
        value = call(
            host,
            "execution.get",
            json!({"execution_id":id,"after_seq":value["sequence"],"wait_ms":100}),
        )
        .unwrap();
    }
}

#[test]
fn bounded_wait_deduplicates_and_retains_admission_until_completion() {
    let temp = tempfile::tempdir().unwrap();
    let supervisor = Arc::new(InstrumentedWorkerSupervisor::with_delay(180));
    let host = ServiceHost::new(supervisor.clone());
    let opened = open(&host, temp.path());
    let sid = &opened["session_id"];
    let input = json!({"session_id":sid,"command":".echo once","wait_ms":0,"client_request_id":"once","expected_service_instance":opened["service_instance"]});
    let first = call(&host, "session.exec", input.clone()).unwrap();
    assert_eq!(first["wait_expired"], true);
    let repeated = call(&host, "session.exec", input.clone()).unwrap();
    assert_eq!(first["execution_id"], repeated["execution_id"]);
    assert!(
        call(
            &host,
            "session.exec",
            json!({"session_id":sid,"command":".echo busy"})
        )
        .is_err()
    );
    let mut conflict = input.clone();
    conflict["command"] = json!(".echo different");
    assert!(call(&host, "session.exec", conflict).is_err());
    let mut restarted = input.clone();
    restarted["expected_service_instance"] = json!("old-instance");
    assert!(call(&host, "session.exec", restarted).is_err());
    assert_eq!(complete(&host, &first["execution_id"])["status"], "success");
    let repeated = call(&host, "session.exec", input).unwrap();
    assert_eq!(first["execution_id"], repeated["execution_id"]);
    assert_eq!(supervisor.max_active.load(Ordering::SeqCst), 1);
    let facts = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR))
        .unwrap()
        .list_operations()
        .unwrap();
    assert_eq!(
        facts
            .iter()
            .filter(|r| r.capability == "debug.eval")
            .count(),
        1
    );
    call(&host, "session.close", json!({"session_id":sid})).unwrap();
}

#[test]
fn output_cursor_is_utf8_safe_resource_bound_and_survives_close() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let opened = open(&host, temp.path());
    let sid = &opened["session_id"];
    let response = call(
        &host,
        "session.exec",
        json!({"session_id":sid,"command":".echo 调试✓结束"}),
    )
    .unwrap();
    let id = &response["execution_id"];
    let first = call(
        &host,
        "execution.read",
        json!({"execution_id":id,"max_bytes":7}),
    )
    .unwrap();
    assert_eq!(first["complete"], false);
    let cursor = first["next_cursor"].clone();
    let same_a = call(
        &host,
        "execution.read",
        json!({"execution_id":id,"cursor":cursor,"max_bytes":7}),
    )
    .unwrap();
    let same_b = call(
        &host,
        "execution.read",
        json!({"execution_id":id,"cursor":cursor,"max_bytes":7}),
    )
    .unwrap();
    assert_eq!(same_a, same_b);
    assert!(
        call(
            &host,
            "session.read",
            json!({"session_id":sid,"cursor":cursor})
        )
        .is_err()
    );
    call(&host, "session.close", json!({"session_id":sid})).unwrap();
    let mut text = String::new();
    let mut cursor = Value::Null;
    for _ in 0..100 {
        let part = call(
            &host,
            "execution.read",
            json!({"execution_id":id,"cursor":cursor,"max_bytes":4}),
        )
        .unwrap();
        for item in part["items"].as_array().unwrap() {
            if let Some(chunk) = item["text"].as_str() {
                text.push_str(chunk);
            }
        }
        assert_eq!(part["gap"], false);
        cursor = part["next_cursor"].clone();
        if part["complete"] == true {
            break;
        }
    }
    assert!(text.contains("调试✓结束"));
    assert!(!text.contains('\u{fffd}'));
    let execution = call(&host, "execution.get", json!({"execution_id":id})).unwrap();
    assert_eq!(execution["status"], "success");
    assert!(execution.get("events").is_none());
}

#[test]
fn close_is_idempotent_coordinates_execution_and_keeps_control_result_separate() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(InstrumentedWorkerSupervisor::wait_for_cancel()));
    let opened = open(&host, temp.path());
    let sid = &opened["session_id"];
    let active = call(
        &host,
        "session.exec",
        json!({"session_id":sid,"command":".echo pending","wait_ms":0}),
    )
    .unwrap();
    let close = call(
        &host,
        "session.close",
        json!({"session_id":sid,"wait_ms":0}),
    )
    .unwrap();
    let retry = call(
        &host,
        "session.close",
        json!({"session_id":sid,"wait_ms":0}),
    )
    .unwrap();
    assert_eq!(close["execution_id"], retry["execution_id"]);
    assert!(
        call(
            &host,
            "session.exec",
            json!({"session_id":sid,"command":".echo forbidden"})
        )
        .is_err()
    );
    assert_eq!(
        complete(&host, &active["execution_id"])["status"],
        "success"
    );
    assert_eq!(complete(&host, &close["execution_id"])["status"], "success");
    assert_eq!(
        call(&host, "session.get", json!({"session_id":sid})).unwrap()["lifecycle"],
        "closed"
    );
    let cancel = call(
        &host,
        "execution.cancel",
        json!({"execution_id":active["execution_id"]}),
    )
    .unwrap();
    assert_eq!(cancel["code"], "already_finished");
    assert_eq!(cancel["execution"]["status"], "success");
    assert_ne!(
        cancel["control_execution"]["execution_id"],
        active["execution_id"]
    );
}

#[test]
fn artifact_provenance_and_input_reuse_do_not_change_producer() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let opened = open(&host, temp.path());
    let sid = &opened["session_id"];
    let response = call(
        &host,
        "session.exec",
        json!({"session_id":sid,"command":".echo material"}),
    )
    .unwrap();
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let reference: ArtifactRef =
        serde_json::from_value(response["execution"]["raw_output_ref"].clone()).unwrap();
    let before = workspace.get_artifact(&reference).unwrap().unwrap();
    assert_eq!(json!(before.producer_session_id), *sid);
    assert_eq!(
        json!(before.producer_execution_id),
        response["execution_id"]
    );
    let reopened = call(
        &host,
        "session.open",
        json!({"project_root":temp.path(),"target":{"kind":"file","artifact_ref":reference}}),
    )
    .unwrap();
    assert_eq!(reopened["session"]["input_artifacts"], json!([reference]));
    let after = workspace.get_artifact(&reference).unwrap().unwrap();
    assert_eq!(before.producer_session_id, after.producer_session_id);
    assert_eq!(before.producer_execution_id, after.producer_execution_id);
    assert!(call(&host,"session.open",json!({"project_root":temp.path(),"target":{"kind":"file","artifact_ref":reference,"path":"also.dmp"}})).is_err());
    let page = call(
        &host,
        "execution.list",
        json!({"project_root":temp.path(),"limit":1}),
    )
    .unwrap();
    assert_eq!(page["has_more"], true);
    let next = call(
        &host,
        "execution.list",
        json!({"project_root":temp.path(),"limit":1,"cursor":page["next_cursor"]}),
    )
    .unwrap();
    assert_ne!(
        page["executions"][0]["execution_id"],
        next["executions"][0]["execution_id"]
    );
}

#[test]
fn successful_execution_with_failed_registration_remains_success_and_output_gap() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(BadEvalWriteSupervisor));
    let opened = open(&host, temp.path());
    let result = call(
        &host,
        "session.exec",
        json!({"session_id":opened["session_id"],"command":".echo once"}),
    )
    .unwrap();
    assert_eq!(result["execution"]["status"], "success");
    assert!(result["execution"]["output_error"].is_string());
    assert_eq!(result["execution"]["output_complete"], false);
}

#[test]
fn output_loss_stays_visible_across_cursors_and_invalid_utf8_tails() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let opened = open(&host, temp.path());
    let response = call(
        &host,
        "session.exec",
        json!({"session_id":opened["session_id"],"command":".echo fixture"}),
    )
    .unwrap();
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let reference =
        serde_json::from_value(response["execution"]["raw_output_ref"].clone()).unwrap();
    let metadata = workspace.get_artifact(&reference).unwrap().unwrap();
    let path = workspace
        .resolve_artifact_relative_path(&metadata.relative_path)
        .unwrap();
    // Invalid byte followed by a four-byte character crossing the read budget.
    fs::write(path, [255, 240, 159, 152, 128, b'x']).unwrap();
    let first = call(
        &host,
        "execution.read",
        json!({"execution_id":response["execution_id"],"max_bytes":4}),
    )
    .unwrap();
    assert_eq!(first["gap"], true);
    assert_eq!(first["items"][0]["text"], "�");
    let next=call(&host,"execution.read",json!({"execution_id":response["execution_id"],"cursor":first["next_cursor"],"max_bytes":4})).unwrap();
    assert_eq!(next["items"][0]["text"], "😀");
    assert_eq!(next["gap"], true);
    let end = call(
        &host,
        "execution.read",
        json!({"execution_id":response["execution_id"],"cursor":next["next_cursor"],"max_bytes":4}),
    )
    .unwrap();
    assert_eq!(end["gap"], true);
    assert_eq!(end["complete"], false);
    assert_eq!(end["has_more"], false);
}

#[test]
fn execution_get_waits_without_sequence_and_batch_children_keep_parent() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(InstrumentedWorkerSupervisor::with_delay(150)));
    let opened = open(&host, temp.path());
    let accepted=call(&host,"debug.eval_steps",json!({"session_id":opened["session_id"],"commands":[".echo first",".echo second"],"wait_ms":0})).unwrap();
    let final_value = call(
        &host,
        "execution.get",
        json!({"execution_id":accepted["execution_id"],"wait_ms":30000}),
    )
    .unwrap();
    assert_eq!(final_value["status"], "success");
    assert_eq!(final_value["batch_result"]["unexecuted_steps"], json!([]));
    let children = final_value["batch_result"]["steps"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    for child in children {
        let value = call(
            &host,
            "execution.get",
            json!({"execution_id":child["execution_id"]}),
        )
        .unwrap();
        assert_eq!(value["parent_execution_id"], accepted["execution_id"]);
    }
}

#[test]
fn legacy_deadline_is_rejected_before_acceptance_and_stream_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let opened = open(&host, temp.path());
    let error = call(
        &host,
        "debug.eval",
        json!({"session_id":opened["session_id"],"command":".echo must-not-run","timeout_ms":100}),
    )
    .unwrap_err();
    assert!(error.message.contains("wait_ms"));
    let response = call(
        &host,
        "session.exec",
        json!({"session_id":opened["session_id"],"command":".echo output"}),
    )
    .unwrap();
    let stream = call(
        &host,
        "operation.stream",
        json!({"operation_id":response["execution_id"],"max_bytes":4}),
    )
    .unwrap();
    assert!(stream.get("events").is_none());
    assert_eq!(stream["has_more"], true);
    assert!(stream["next_cursor"].is_string());
}
