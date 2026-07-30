use super::*;

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
    let host = ServiceHost::with_mock_workers();
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
        assert_eq!(result["artifact_refs"].as_array().unwrap().len(), 2);
        assert!(result["result_delivery"]["full_artifact_ref"]["id"].is_string());
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
    assert!(
        artifacts
            .iter()
            .filter(|artifact| artifact.kind == "reverse.result")
            .count()
            >= 25
    );
    let audit = artifacts
        .iter()
        .find(|artifact| artifact.kind == "reverse.core")
        .unwrap();
    let audit_path = workspace
        .resolve_artifact_relative_path(&audit.relative_path)
        .unwrap();
    let audit_event: Value =
        serde_json::from_str(&fs::read_to_string(audit_path).unwrap()).unwrap();
    assert!(audit_event.get("result").is_none());
    assert!(audit_event["result_artifact_ref"]["id"].is_string());
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
    assert_eq!(declare["result"]["atomic"], true);
    assert_eq!(declare["result"]["changed_count"], 0);
    assert_eq!(declare["result"]["errors"], 1);
    assert_eq!(declare["result"]["items"][0]["ok"], false);
    assert_eq!(declare["result"]["items"][0]["committed"], false);
    assert_eq!(declare["result"]["items"][1]["ok"], false);
    assert!(
        declare["result"]["items"][1]["hint"]
            .as_str()
            .unwrap()
            .contains("no declarations were committed")
    );
}

#[test]
fn reverse_declare_type_preserves_function_pointer_parameter_commas() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());
    let declaration = r#"typedef unsigned int (__fastcall *CEVA_ApiHandler_t)(
        void *engine_ctx,
        const unsigned int *args,
        CEVA_GuestCpuState32_t *guest,
        const CEVA_ApiCallContext_t *call_ctx);"#;

    let response = reverse_core_rpc(
        &host,
        "reverse.declare_type",
        session_id,
        json!({ "decls": declaration }),
    );

    assert!(response.error.is_none(), "{:?}", response.error);
    let result = &response.result.unwrap()["result"];
    assert_eq!(result["ok"], true);
    assert_eq!(result["atomic"], true);
    assert_eq!(result["count"], 1);
    assert_eq!(result["changed_count"], 1);
    assert_eq!(result["items"][0]["decl"], declaration);
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
fn reverse_decompile_large_result_returns_preview_with_artifact_ref() {
    let result = json!({
        "addr": 0x140001000u64,
        "function_name": "big_function",
        "language": "c",
        "pseudocode": "x".repeat(DEFAULT_INLINE_RESULT_BYTE_LIMIT * 2),
    });
    let full_byte_len = serde_json::to_vec(&result).unwrap().len() as u64;
    let preview =
        json_result_preview(&result, full_byte_len, DEFAULT_INLINE_RESULT_BYTE_LIMIT).unwrap();

    assert!(preview.truncated);
    assert_eq!(preview.full_byte_len, full_byte_len);
    assert!(serde_json::to_vec(&preview.value).unwrap().len() <= DEFAULT_INLINE_RESULT_BYTE_LIMIT);
    assert!(preview.value.get("pseudocode_truncated").is_none());
    assert!(preview.value.get("full_result_artifact_ref").is_none());
}

#[test]
fn reverse_decompile_over_one_mib_uses_bounded_preview_and_complete_json_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());
    let response = reverse_core_rpc(
        &host,
        "reverse.decompile",
        session_id,
        json!({ "addr": "0x1400ffff0" }),
    );
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["result_delivery"]["truncated"], true);
    assert!(serde_json::to_vec(&result).unwrap().len() < MAX_WORKER_RESPONSE_LINE_BYTES);

    let artifact_ref: ArtifactRef =
        serde_json::from_value(result["result_delivery"]["full_artifact_ref"].clone()).unwrap();
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifact = workspace.get_artifact(&artifact_ref).unwrap().unwrap();
    assert_eq!(artifact.kind, "reverse.result");
    let path = workspace
        .resolve_artifact_relative_path(artifact.relative_path)
        .unwrap();
    let full: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(full["pseudocode"].as_str().unwrap().len(), 2 * 1024 * 1024);
}

#[test]
fn reverse_disasm_keeps_all_instructions_in_artifact_and_bounds_inline_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::with_mock_workers();
    let session_id = open_reverse_session(&host, temp.path());
    let response = reverse_core_rpc(
        &host,
        "reverse.disasm",
        session_id,
        json!({ "addr": "0x1400ffee0" }),
    );
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["result_delivery"]["truncated"], true);
    let inline_count = result["result"]["instructions"].as_array().unwrap().len();
    assert!(inline_count > 256);
    assert!(inline_count < 5_000);

    let artifact_ref: ArtifactRef =
        serde_json::from_value(result["result_delivery"]["full_artifact_ref"].clone()).unwrap();
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    let artifact = workspace.get_artifact(&artifact_ref).unwrap().unwrap();
    let path = workspace
        .resolve_artifact_relative_path(artifact.relative_path)
        .unwrap();
    let full: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(full["instruction_count"], 5_000);
    assert_eq!(full["instructions"].as_array().unwrap().len(), 5_000);
}

#[test]
fn reverse_artifact_registration_failure_finalizes_operation() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(BadEvalWriteSupervisor));
    let session_id = open_reverse_session(&host, temp.path());

    let response = reverse_core_rpc(
        &host,
        "reverse.disasm",
        session_id,
        json!({ "addr": "0x140001000" }),
    );
    assert!(response.error.unwrap().message.contains("artifact path"));

    let state = host.lock_state().unwrap();
    assert!(state.operations.values().any(|operation| {
        operation.capability == "reverse.disasm"
            && operation.status == ServiceOperationStatus::Failed
    }));
    drop(state);
    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    assert!(
        workspace
            .list_operations()
            .unwrap()
            .iter()
            .any(|operation| {
                operation.capability == "reverse.disasm"
                    && matches!(operation.status, OperationStatus::Failed)
            })
    );
}

#[test]
fn reverse_transport_failure_removes_partial_result() {
    let temp = tempfile::tempdir().unwrap();
    let host = ServiceHost::new(Arc::new(ArtifactThenTransportFailureSupervisor));
    let session_id = open_reverse_session(&host, temp.path());

    let response = reverse_core_rpc(
        &host,
        "reverse.disasm",
        session_id,
        json!({ "addr": "0x140001000" }),
    );
    assert!(response.error.is_some());

    let workspace = Workspace::open(temp.path().join(INTERNAL_WORKSPACE_DIR)).unwrap();
    assert!(
        workspace
            .list_artifacts()
            .unwrap()
            .iter()
            .all(|artifact| artifact.kind != "reverse.result")
    );
    let results = workspace
        .root()
        .join("artifacts")
        .join("reverse_sessions")
        .read_dir()
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("results");
    assert!(results.read_dir().unwrap().next().is_none());
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
