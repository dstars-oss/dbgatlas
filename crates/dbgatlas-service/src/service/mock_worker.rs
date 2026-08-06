use super::*;

pub(super) fn mock_worker_response(request: WorkerRequest) -> Result<WorkerResponse, ServiceError> {
    match request {
        WorkerRequest::StartDebugSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session started by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::EvalDebugCommand {
            session_id,
            operation_id,
            command,
            artifact_dir,
        } => write_mock_eval_response(session_id, operation_id, command, artifact_dir),
        WorkerRequest::AddSymbols {
            session_id,
            operation_id,
            symbol_path,
            reload,
            artifact_dir,
        } => {
            let command = if reload {
                format!(".sympath+ {symbol_path}; .reload")
            } else {
                format!(".sympath+ {symbol_path}")
            };
            write_mock_eval_response(session_id, operation_id, command, artifact_dir)
        }
        WorkerRequest::ReadMemory {
            session_id,
            operation_id,
            address,
            length,
            artifact_dir,
        } => write_mock_memory_response(session_id, operation_id, address, length, artifact_dir),
        WorkerRequest::OpenReverseSession { .. } => Ok(WorkerResponse::ReverseSessionOpened {
            runtime: ReverseRuntimeInfo {
                runtime_major: 9,
                runtime_minor: 3,
                runtime_build: 0,
                provider_file_name: "dbgatlas_ida_sdk_930.dll".to_string(),
                ida_sdk_version: 930,
                abi_major: 0,
                abi_minor: 3,
                abi_patch: 0,
            },
            writes: Vec::new(),
        }),
        WorkerRequest::LookupReverseFunction {
            runtime_address,
            runtime_module_base,
            ida_image_base,
            ..
        } => {
            if runtime_address < runtime_module_base {
                return Ok(WorkerResponse::Failed {
                    code: "reverse_lookup_failed".to_string(),
                    message: "runtime_address is below runtime_module_base".to_string(),
                    writes: Vec::new(),
                });
            }
            let rva = runtime_address - runtime_module_base;
            let ida_ea = match ida_image_base.checked_add(rva) {
                Some(value) => value,
                None => {
                    return Ok(WorkerResponse::Failed {
                        code: "reverse_lookup_failed".to_string(),
                        message: "ida_ea overflow".to_string(),
                        writes: Vec::new(),
                    });
                }
            };
            Ok(WorkerResponse::ReverseFunctionLookup {
                result: ReverseFunctionLookupResult {
                    runtime_address,
                    runtime_module_base,
                    rva,
                    ida_image_base,
                    ida_ea,
                    function_start: ida_ea & !0xff,
                    function_end: (ida_ea & !0xff) + 0x100,
                    function_name: format!("mock_function_{:x}", ida_ea & !0xff),
                    found: true,
                },
                writes: Vec::new(),
            })
        }
        WorkerRequest::ReverseCoreFunction {
            session_id,
            operation_id,
            function,
            arguments,
            artifact_dir,
        } => {
            mock_reverse_core_response(session_id, operation_id, function, arguments, artifact_dir)
        }
        WorkerRequest::CloseReverseSession { .. } => Ok(WorkerResponse::Ok {
            summary: "reverse session closed by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::CloseSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session closed by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::KillSession { .. } => Ok(WorkerResponse::Ok {
            summary: "debug session killed by mock worker".to_string(),
            writes: Vec::new(),
        }),
        WorkerRequest::CancelOperation { .. } => Ok(WorkerResponse::Ok {
            summary: "operation canceled by mock worker".to_string(),
            writes: Vec::new(),
        }),
    }
}

pub(super) fn mock_reverse_core_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    function: String,
    arguments: Value,
    artifact_dir: PathBuf,
) -> Result<WorkerResponse, ServiceError> {
    let result = match function.as_str() {
        "lookup_funcs" => mock_lookup_funcs(&arguments)?,
        "int_convert" => mock_int_convert(&arguments),
        "list_funcs" => mock_list_funcs(&arguments)?,
        "list_globals" => mock_list_globals(&arguments)?,
        "list_imports" => mock_list_imports(&arguments)?,
        "list_strings" => mock_list_strings(&arguments)?,
        "get_string" => mock_get_string(&arguments)?,
        "get_bytes" => mock_get_bytes(&arguments)?,
        "get_int" => mock_get_int(&arguments)?,
        "decompile" => mock_decompile(&arguments)?,
        "disasm" => mock_disasm(&arguments)?,
        "xrefs_to" => mock_xrefs_to(&arguments)?,
        "xrefs_to_field" => mock_xrefs_to_field(&arguments)?,
        "callees" => mock_callees(&arguments)?,
        "rename" => mock_batch_write_result_with_diagnostics(&arguments, "items")?,
        "set_comments" => mock_batch_write_result(&arguments, "items")?,
        "set_type" => mock_set_type_result(&arguments)?,
        "declare_type" => mock_declare_type(&arguments),
        "inspect_item" => mock_inspect_item(&arguments)?,
        "force_recompile" => mock_force_recompile(&arguments)?,
        "idb_save" => mock_idb_save(&arguments),
        "py_eval" => mock_py_eval(&arguments),
        "find_bytes" => mock_find_bytes(&arguments)?,
        "search_text" => mock_search_text(&arguments)?,
        "query_xrefs" => mock_query_xrefs(&arguments)?,
        "query_funcs" => mock_query_funcs(&arguments)?,
        "query_entities" => mock_query_entities(&arguments)?,
        other => {
            return Ok(WorkerResponse::Failed {
                code: "reverse_core_failed".to_string(),
                message: format!("unsupported IDA Core Function `{other}`"),
                writes: Vec::new(),
            });
        }
    };
    let bytes = serde_json::to_vec(&result)?;
    let result_path = artifact_dir
        .join("results")
        .join(format!("{}.json", operation_id.id.as_str()));
    write_atomic_bytes(&result_path, &bytes)?;
    let preview = json_result_preview(
        &result,
        bytes.len() as u64,
        DEFAULT_INLINE_RESULT_BYTE_LIMIT,
    )?;
    Ok(WorkerResponse::ReverseCoreFunction {
        result: ReverseCoreFunctionResult {
            function,
            result: preview,
            warnings: Vec::new(),
        },
        writes: vec![WorkerArtifactWrite {
            relative_path: reverse_relative_path(
                &session_id,
                &format!("results/{}.json", operation_id.id.as_str()),
            ),
            kind: "reverse.result".to_string(),
            role: Some(WorkerArtifactRole::FullResult),
            byte_len: bytes.len() as u64,
            description: Some("complete mock IDA Core Function result".to_string()),
        }],
    })
}

pub(super) fn mock_lookup_funcs(arguments: &Value) -> Result<Value, ServiceError> {
    let queries = normalize_core_list(arguments.get("queries").unwrap_or(&Value::Null));
    let runtime_module_base = optional_u64_argument(arguments, "runtime_module_base")?.unwrap_or(0);
    let ida_image_base = optional_u64_argument(arguments, "ida_image_base")?.unwrap_or(0);
    let mut items = Vec::new();
    for query in queries {
        if looks_like_ttd_position(&query) {
            return Err(ServiceError::Rpc(
                "queries contains a TTD position, not a code address".to_string(),
            ));
        }
        if let Some((ida_ea, normalized)) = parse_mock_module_rva_address(&query)? {
            let start = ida_ea & !0xff;
            items.push(json!({
                "query": query,
                "input_type": "address",
                "found": true,
                "ida_ea": ida_ea,
                "function_start": start,
                "function_end": start + 0x100,
                "function_name": format!("mock_function_{start:x}"),
                "normalized_address": normalized
            }));
            continue;
        }
        if let Some(address) = parse_optional_u64_text(&query) {
            if address < runtime_module_base {
                return Err(ServiceError::Rpc(
                    "runtime address is below runtime_module_base".to_string(),
                ));
            }
            let rva = address - runtime_module_base;
            let ida_ea = ida_image_base
                .checked_add(rva)
                .ok_or_else(|| ServiceError::Rpc("ida_ea overflow".to_string()))?;
            let start = ida_ea & !0xff;
            items.push(json!({
                "query": query,
                "input_type": "address",
                "found": true,
                "runtime_address": address,
                "runtime_module_base": runtime_module_base,
                "rva": rva,
                "ida_image_base": ida_image_base,
                "ida_ea": ida_ea,
                "function_start": start,
                "function_end": start + 0x100,
                "function_name": format!("mock_function_{start:x}"),
                "normalized_address": {
                    "input": query,
                    "kind": if runtime_module_base != 0 || ida_image_base != 0 { "runtime_va" } else { "ida_ea" },
                    "ida_ea": ida_ea,
                    "confidence": "high"
                }
            }));
        } else {
            let found = !query.contains("missing");
            items.push(json!({
                "query": query,
                "input_type": "name",
                "found": found,
                "function_start": if found { json!(0x140001000u64) } else { Value::Null },
                "function_end": if found { json!(0x140001100u64) } else { Value::Null },
                "function_name": if found { json!(query) } else { Value::Null }
            }));
        }
    }
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

pub(super) fn mock_int_convert(arguments: &Value) -> Value {
    let inputs = normalize_core_list(arguments.get("inputs").unwrap_or(arguments));
    let items: Vec<Value> = inputs
        .into_iter()
        .map(|input| match parse_core_integer(&input) {
            Some(value) => json!({
                "input": input,
                "decimal": value.to_string(),
                "hex": format!("0x{value:x}"),
                "binary": format!("0b{value:b}"),
                "bytes_le": value.to_le_bytes().to_vec(),
                "ascii": ascii_from_core_integer(value)
            }),
            None => json!({
                "input": input,
                "error": "not a supported integer, hex, binary, bytes, or ASCII representation"
            }),
        })
        .collect();
    let count = items.len();
    json!({ "items": items, "count": count })
}

pub(super) fn mock_list_funcs(arguments: &Value) -> Result<Value, ServiceError> {
    let rows = vec![
        json!({ "address": 0x140001000u64, "name": "main", "size": 0x80 }),
        json!({ "address": 0x140001100u64, "name": "parse_args", "size": 0x60 }),
        json!({ "address": 0x140001200u64, "name": "dispatch_command", "size": 0xa0 }),
        json!({ "address": 0x140001300u64, "name": "cleanup", "size": 0x40 }),
    ];
    paginate_filtered(rows, arguments)
}

pub(super) fn mock_list_globals(arguments: &Value) -> Result<Value, ServiceError> {
    let rows = vec![
        json!({ "address": 0x140020000u64, "name": "g_runtime_config", "type": "struct RuntimeConfig" }),
        json!({ "address": 0x140020080u64, "name": "g_worker_state", "type": "struct WorkerState" }),
    ];
    paginate_filtered(rows, arguments)
}

pub(super) fn mock_list_imports(arguments: &Value) -> Result<Value, ServiceError> {
    let rows = vec![
        json!({ "module": "KERNEL32.dll", "name": "CreateFileW", "ordinal": Value::Null, "iat_ea": 0x140030000u64 }),
        json!({ "module": "KERNEL32.dll", "name": "ReadFile", "ordinal": Value::Null, "iat_ea": 0x140030008u64 }),
        json!({ "module": "USER32.dll", "name": "MessageBoxW", "ordinal": Value::Null, "iat_ea": 0x140030010u64 }),
    ];
    paginate_filtered(rows, arguments)
}

pub(super) fn mock_list_strings(arguments: &Value) -> Result<Value, ServiceError> {
    let rows = vec![
        json!({ "address": 0x140040000u64, "length": 18, "type": 0, "text": "https://example.test" }),
        json!({ "address": 0x140040020u64, "length": 13, "type": 0, "text": "config_path" }),
        json!({ "address": 0x140040040u64, "length": 17, "type": 0, "text": "CreateFileW failed" }),
    ];
    paginate_filtered(rows, arguments)
}

pub(super) fn mock_get_string(arguments: &Value) -> Result<Value, ServiceError> {
    let (addr, normalized) = required_mock_code_address_argument(arguments, "addr")?;
    let length = optional_u64_argument(arguments, "length")?;
    if let Some(length) = length
        && (length == 0 || length > 4096)
    {
        return Err(ServiceError::Rpc(
            "length must be between 1 and 4096 bytes".to_string(),
        ));
    }
    let string_type = optional_u64_argument(arguments, "type")?.unwrap_or(0);
    let text = match addr {
        0x140040000 => Some("https://example.test"),
        0x140040020 => Some("config_path"),
        0x140040040 => Some("CreateFileW failed"),
        _ => None,
    };
    Ok(json!({
        "address": addr,
        "normalized_address": normalized,
        "found": text.is_some(),
        "length": length.unwrap_or_else(|| text.map_or(0, str::len) as u64),
        "type": string_type,
        "text": text
    }))
}

pub(super) fn mock_get_bytes(arguments: &Value) -> Result<Value, ServiceError> {
    let (addr, normalized) = required_mock_code_address_argument(arguments, "addr")?;
    let length = required_u64_argument(arguments, "length")?;
    let bytes = mock_idb_bytes(addr, length)?;
    Ok(json!({
        "address": addr,
        "normalized_address": normalized,
        "requested_length": length,
        "read_length": bytes.len(),
        "complete": bytes.len() as u64 == length,
        "bytes_hex": hex_encode(&bytes)
    }))
}

pub(super) fn mock_get_int(arguments: &Value) -> Result<Value, ServiceError> {
    let (addr, normalized) = required_mock_code_address_argument(arguments, "addr")?;
    let size = optional_u64_argument(arguments, "size")?.unwrap_or(8);
    if !matches!(size, 1 | 2 | 4 | 8) {
        return Err(ServiceError::Rpc(
            "size must be one of 1, 2, 4, or 8 bytes".to_string(),
        ));
    }
    let endian = arguments
        .get("endian")
        .and_then(Value::as_str)
        .unwrap_or("little");
    if !matches!(endian, "little" | "big") {
        return Err(ServiceError::Rpc(
            "endian must be `little` or `big`".to_string(),
        ));
    }
    let bytes = mock_idb_bytes(addr, size)?;
    let mut value = 0u64;
    if endian == "little" {
        for (index, byte) in bytes.iter().enumerate() {
            value |= (*byte as u64) << (index * 8);
        }
    } else {
        for byte in &bytes {
            value = (value << 8) | (*byte as u64);
        }
    }
    Ok(json!({
        "address": addr,
        "normalized_address": normalized,
        "size": size,
        "endian": endian,
        "complete": true,
        "bytes_hex": hex_encode(&bytes),
        "decimal": value.to_string(),
        "hex": format!("0x{value:x}")
    }))
}

pub(super) fn mock_decompile(arguments: &Value) -> Result<Value, ServiceError> {
    let (addr, normalized) = required_mock_code_address_argument(arguments, "addr")?;
    let start = addr & !0xff;
    let pseudocode = if addr == 0x1400_ffff0 {
        "x".repeat(2 * 1024 * 1024)
    } else {
        format!("int mock_function_{start:x}(void) {{\n    return 0;\n}}")
    };
    Ok(json!({
        "addr": addr,
        "normalized_address": normalized,
        "function_start": start,
        "function_name": format!("mock_function_{start:x}"),
        "language": "c",
        "pseudocode": pseudocode
    }))
}

pub(super) fn mock_disasm(arguments: &Value) -> Result<Value, ServiceError> {
    let (addr, normalized) = required_mock_code_address_argument(arguments, "addr")?;
    let start = addr & !0xff;
    let instructions = if addr == 0x1400_ffee0 {
        (0..5_000u64)
            .map(|index| {
                json!({
                    "ea": start + index,
                    "text": format!("mov rax, qword ptr [rcx+0x{index:x}]")
                })
            })
            .collect::<Vec<_>>()
    } else {
        vec![
            json!({ "ea": start, "text": "push rbp" }),
            json!({ "ea": start + 1, "text": "mov rbp, rsp" }),
            json!({ "ea": start + 4, "text": "ret" }),
        ]
    };
    Ok(json!({
        "addr": addr,
        "normalized_address": normalized,
        "function_start": start,
        "function_name": format!("mock_function_{start:x}"),
        "arguments": [{ "name": "arg_0", "location": "rcx", "type": "uint64_t" }],
        "stack_frame": { "size": 0x40, "locals": [{ "name": "var_8", "offset": -8, "size": 8 }] },
        "instruction_count": instructions.len(),
        "instructions": instructions
    }))
}

pub(super) fn mock_xrefs_to(arguments: &Value) -> Result<Value, ServiceError> {
    let addrs = normalize_core_list(arguments.get("addrs").unwrap_or(&Value::Null));
    let mut items = Vec::new();
    for addr in addrs {
        let (ea, normalized) = parse_mock_code_address_text(&addr)?;
        items.push(json!({
            "to": ea,
            "normalized_address": normalized,
            "xrefs": [
                { "from": ea.saturating_sub(0x20), "type": "code", "function": format!("mock_function_{:x}", ea.saturating_sub(0x20) & !0xff) }
            ]
        }));
    }
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

pub(super) fn mock_xrefs_to_field(arguments: &Value) -> Result<Value, ServiceError> {
    let queries = normalize_core_list(arguments.get("queries").unwrap_or(&Value::Null));
    let items: Vec<Value> = queries
        .into_iter()
        .map(|query| {
            json!({
                "query": query,
                "xrefs": [
                    { "from": 0x140001240u64, "operand": 1, "access": "read" }
                ]
            })
        })
        .collect();
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

pub(super) fn mock_callees(arguments: &Value) -> Result<Value, ServiceError> {
    let addrs = normalize_core_list(arguments.get("addrs").unwrap_or(&Value::Null));
    let mut items = Vec::new();
    for addr in addrs {
        let (ea, normalized) = parse_mock_code_address_text(&addr)?;
        items.push(json!({
            "function": ea,
            "normalized_address": normalized,
            "callees": [
                { "ea": 0x140001100u64, "name": "parse_args" },
                { "ea": 0x140001200u64, "name": "dispatch_command" }
            ]
        }));
    }
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

pub(super) fn mock_batch_write_result(
    arguments: &Value,
    field: &'static str,
) -> Result<Value, ServiceError> {
    let items = arguments
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            arguments
                .get(field)
                .map(|value| vec![value.clone()])
                .unwrap_or_default()
        });
    let mut results = Vec::new();
    for item in items {
        let (_, normalized) = mock_item_addr_and_normalized(&item)?;
        let mut result = json!({
                "input": item,
                "ok": true
        });
        if let Some(normalized) = normalized {
            result["normalized_address"] = normalized;
        }
        results.push(result);
    }
    let count = results.len();
    Ok(json!({
        "items": results,
        "count": count,
        "changed_count": count
    }))
}

pub(super) fn mock_batch_write_result_with_diagnostics(
    arguments: &Value,
    field: &'static str,
) -> Result<Value, ServiceError> {
    let items = arguments
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            arguments
                .get(field)
                .map(|value| vec![value.clone()])
                .unwrap_or_default()
        });
    let mut results = Vec::new();
    for item in items {
        let (addr, normalized) = mock_item_addr_and_normalized(&item)?;
        let mut result = json!({
                "input": item,
                "ea": addr,
                "ok": true,
                "item_state": mock_item_state(addr)
        });
        if let Some(normalized) = normalized {
            result["normalized_address"] = normalized;
        }
        results.push(result);
    }
    let count = results.len();
    Ok(json!({
        "items": results,
        "count": count,
        "changed_count": count
    }))
}

pub(super) fn mock_set_type_result(arguments: &Value) -> Result<Value, ServiceError> {
    let items = arguments
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            arguments
                .get("items")
                .map(|value| vec![value.clone()])
                .unwrap_or_default()
        });
    let mut results = Vec::new();
    for item in items {
        let (addr, normalized) = mock_item_addr_and_normalized(&item)?;
        let type_text = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let mut result = json!({
                "input": item,
                "ea": addr,
                "ok": true,
                "generated_decl": mock_generated_decl(type_text),
                "item_state": mock_item_state(addr)
        });
        if let Some(normalized) = normalized {
            result["normalized_address"] = normalized;
        }
        results.push(result);
    }
    let count = results.len();
    Ok(json!({
        "items": results,
        "count": count,
        "changed_count": count
    }))
}

pub(super) fn mock_item_addr_and_normalized(
    item: &Value,
) -> Result<(u64, Option<Value>), ServiceError> {
    let Some(addr) = item.get("addr") else {
        return Ok((0x140020000, None));
    };
    match addr {
        Value::Number(number) => {
            let address = number
                .as_u64()
                .ok_or_else(|| ServiceError::Rpc("addr must be an address".to_string()))?;
            Ok((address, Some(mock_ida_ea_normalized_address(addr, address))))
        }
        Value::String(text) => {
            parse_mock_code_address_text(text).map(|(ea, normalized)| (ea, Some(normalized)))
        }
        _ => Err(ServiceError::Rpc("addr must be an address".to_string())),
    }
}

pub(super) fn mock_item_state(addr: u64) -> Value {
    json!({
        "ea": addr,
        "item_head": addr,
        "item_end": addr + 8,
        "item_size": 8,
        "is_item_head": true,
        "name": "mock_symbol",
        "head_name": "mock_symbol",
        "is_code": false,
        "is_data": true
    })
}

pub(super) fn mock_generated_decl(type_text: &str) -> String {
    let trimmed = type_text.trim();
    if let Some(array_pos) = trimmed.find('[') {
        let (before_array, array_suffix) = trimmed.split_at(array_pos);
        format!("{} mock_symbol{};", before_array.trim(), array_suffix)
    } else {
        format!("{trimmed} mock_symbol;")
    }
}

pub(super) fn mock_declare_type(arguments: &Value) -> Value {
    let decls = normalize_declaration_blocks(arguments.get("decls").unwrap_or(&Value::Null));
    let ok = decls
        .iter()
        .all(|decl| !decl.to_ascii_lowercase().contains("invalid"));
    let errors = usize::from(!ok);
    let items: Vec<Value> = decls
        .iter()
        .enumerate()
        .map(|(index, decl)| {
            let mut item = json!({
                "index": index,
                "ok": ok,
                "committed": ok,
                "errors": errors,
                "decl": decl
            });
            if !ok {
                item["hint"] = json!(
                    "IDA failed to parse the declaration block; no declarations were committed"
                );
            }
            item
        })
        .collect();
    json!({
        "ok": ok,
        "atomic": true,
        "count": decls.len(),
        "changed_count": if ok { decls.len() } else { 0 },
        "errors": errors,
        "diagnostics": if ok {
            Vec::<String>::new()
        } else {
            vec!["mock parser: invalid declaration".to_string()]
        },
        "items": items
    })
}

fn normalize_declaration_blocks(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.trim().to_string(),
                other => other.to_string(),
            })
            .filter(|item| !item.is_empty())
            .collect(),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text.to_string()]
            }
        }
        Value::Null => Vec::new(),
        other => vec![other.to_string()],
    }
}

pub(super) fn mock_inspect_item(arguments: &Value) -> Result<Value, ServiceError> {
    let queries = normalize_core_list(arguments.get("queries").unwrap_or(&Value::Null));
    let mut items = Vec::new();
    for query in queries {
        let (addr, normalized) = match parse_mock_code_address_text(&query) {
            Ok((addr, normalized)) => (addr, Some(normalized)),
            Err(ServiceError::Rpc(_)) if !looks_like_ttd_position(&query) => (0x140020000, None),
            Err(error) => return Err(error),
        };
        let mut item = json!({
                "query": query,
                "ea": addr,
                "item_state": mock_item_state(addr)
        });
        if let Some(normalized) = normalized {
            item["normalized_address"] = normalized;
        }
        items.push(item);
    }
    let count = items.len();
    Ok(json!({
        "items": items,
        "count": count
    }))
}

pub(super) fn mock_force_recompile(arguments: &Value) -> Result<Value, ServiceError> {
    let addrs = normalize_core_list(arguments.get("addrs").unwrap_or(&Value::Null));
    let mut items = Vec::new();
    for addr in addrs {
        let (ea, normalized) = parse_mock_code_address_text(&addr)?;
        items.push(json!({
            "query": addr,
            "ea": ea & !0xff,
            "ok": true,
            "normalized_address": normalized
        }));
    }
    let count = items.len();
    Ok(json!({
        "items": items,
        "count": count,
        "changed_count": count,
        "all": count == 0
    }))
}

pub(super) fn mock_idb_save(arguments: &Value) -> Value {
    json!({
        "ok": true,
        "path": arguments.get("path").cloned().unwrap_or(Value::Null),
        "changed_count": 1
    })
}

pub(super) fn mock_py_eval(arguments: &Value) -> Value {
    let code = arguments
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "ok": true,
        "stdout": if code.contains("print") { "mock py_eval\n" } else { "" },
        "stderr": "",
        "error": Value::Null
    })
}

pub(super) fn mock_find_bytes(arguments: &Value) -> Result<Value, ServiceError> {
    let patterns = normalize_core_list(arguments.get("patterns").unwrap_or(&Value::Null));
    let rows: Vec<Value> = patterns
        .into_iter()
        .enumerate()
        .map(|(index, pattern)| {
            json!({
                "pattern": pattern,
                "ea": 0x140050000u64 + index as u64
            })
        })
        .collect();
    paginate_filtered(rows, arguments)
}

pub(super) fn mock_search_text(arguments: &Value) -> Result<Value, ServiceError> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let rows = vec![
        json!({ "scope": "strings", "ea": 0x140040020u64, "text": "config_path" }),
        json!({ "scope": "names", "ea": 0x140001100u64, "text": "parse_args" }),
        json!({ "scope": "comments", "ea": 0x140001200u64, "text": "dispatch command handler" }),
    ]
    .into_iter()
    .filter(|row| query.is_empty() || row.to_string().to_ascii_lowercase().contains(&query))
    .collect();
    paginate_filtered(rows, arguments)
}

pub(super) fn mock_query_xrefs(arguments: &Value) -> Result<Value, ServiceError> {
    let target = arguments.get("target").cloned().unwrap_or(Value::Null);
    let normalized = match target.as_str() {
        Some(text) => match parse_mock_code_address_text(text) {
            Ok((_, normalized)) => Some(normalized),
            Err(ServiceError::Rpc(_)) if !looks_like_ttd_position(text) => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    let rows = vec![
        json!({
            "direction": arguments.get("direction").and_then(Value::as_str).unwrap_or("to"),
            "type": "code",
            "from": 0x140001220u64,
            "to": target.clone(),
            "function": { "address": 0x140001200u64, "name": "dispatch_command" }
        }),
        json!({
            "direction": arguments.get("direction").and_then(Value::as_str).unwrap_or("to"),
            "type": "data",
            "from": 0x140020000u64,
            "to": target,
            "function": Value::Null
        }),
    ];
    let mut result = paginate_filtered(rows, arguments)?;
    if let Some(normalized) = normalized {
        result["normalized_address"] = normalized;
    }
    Ok(result)
}

pub(super) fn mock_query_funcs(arguments: &Value) -> Result<Value, ServiceError> {
    mock_list_funcs(arguments)
}

pub(super) fn mock_query_entities(arguments: &Value) -> Result<Value, ServiceError> {
    match arguments
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("functions")
    {
        "functions" => mock_list_funcs(arguments),
        "globals" | "names" => mock_list_globals(arguments),
        "imports" => mock_list_imports(arguments),
        "strings" => mock_list_strings(arguments),
        other => Err(ServiceError::Rpc(format!(
            "unsupported query_entities kind `{other}`"
        ))),
    }
}

pub(super) fn mock_idb_bytes(addr: u64, length: u64) -> Result<Vec<u8>, ServiceError> {
    if length == 0 || length > 4096 {
        return Err(ServiceError::Rpc(
            "length must be between 1 and 4096 bytes".to_string(),
        ));
    }
    Ok((0..length)
        .map(|offset| addr.wrapping_add(offset) as u8)
        .collect())
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

pub(super) fn paginate_filtered(
    rows: Vec<Value>,
    arguments: &Value,
) -> Result<Value, ServiceError> {
    let offset = optional_u64_argument(arguments, "offset")?.unwrap_or(0) as usize;
    let count = optional_u64_argument(arguments, "count")?
        .or(optional_u64_argument(arguments, "limit")?)
        .unwrap_or(50)
        .min(1_000) as usize;
    let filter = arguments
        .get("filter")
        .or_else(|| arguments.get("query"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let filtered: Vec<Value> = rows
        .into_iter()
        .filter(|row| {
            filter
                .as_ref()
                .is_none_or(|needle| row.to_string().to_ascii_lowercase().contains(needle))
        })
        .collect();
    let total = filtered.len();
    let items: Vec<Value> = filtered.into_iter().skip(offset).take(count).collect();
    Ok(json!({
        "offset": offset,
        "count": items.len(),
        "total": total,
        "items": items
    }))
}

pub(super) fn normalize_core_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.trim().to_string(),
                other => other.to_string(),
            })
            .filter(|item| !item.is_empty())
            .collect(),
        Value::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        Value::Null => Vec::new(),
        other => vec![other.to_string()],
    }
}

pub(super) fn required_u64_argument(
    arguments: &Value,
    field: &'static str,
) -> Result<u64, ServiceError> {
    arguments
        .get(field)
        .and_then(parse_u64_value)
        .ok_or_else(|| ServiceError::Rpc(format!("{field} must be an address")))
}

pub(super) fn required_mock_code_address_argument(
    arguments: &Value,
    field: &'static str,
) -> Result<(u64, Value), ServiceError> {
    let value = arguments
        .get(field)
        .ok_or_else(|| ServiceError::Rpc(format!("{field} is required")))?;
    match value {
        Value::Number(number) => {
            let address = number
                .as_u64()
                .ok_or_else(|| ServiceError::Rpc(format!("{field} must be an address")))?;
            Ok((address, mock_ida_ea_normalized_address(value, address)))
        }
        Value::String(text) => parse_mock_code_address_text(text),
        _ => Err(ServiceError::Rpc(format!("{field} must be an address"))),
    }
}

pub(super) fn parse_mock_code_address_text(text: &str) -> Result<(u64, Value), ServiceError> {
    if looks_like_ttd_position(text) {
        return Err(ServiceError::Rpc(
            "address is a TTD position, not a code address".to_string(),
        ));
    }
    if let Some((address, normalized)) = parse_mock_module_rva_address(text)? {
        return Ok((address, normalized));
    }
    if let Some(address) = parse_optional_u64_text(text) {
        return Ok((
            address,
            mock_ida_ea_normalized_address(&json!(text), address),
        ));
    }
    Err(ServiceError::Rpc(
        "address must be an IDA address, integer string, or module+RVA string".to_string(),
    ))
}

pub(super) fn parse_mock_module_rva_address(
    text: &str,
) -> Result<Option<(u64, Value)>, ServiceError> {
    let text = text.trim();
    let Some((module, rva_text)) = text.rsplit_once('+') else {
        return Ok(None);
    };
    let module = module.trim();
    if module.is_empty() || module.contains(':') {
        return Ok(None);
    }
    let Some(rva) = parse_optional_u64_text(rva_text.trim()) else {
        return Ok(None);
    };
    const MOCK_IDA_IMAGE_BASE: u64 = 0x1_4000_0000;
    let ida_ea = MOCK_IDA_IMAGE_BASE
        .checked_add(rva)
        .ok_or_else(|| ServiceError::Rpc("module+RVA overflows IDA address space".to_string()))?;
    Ok(Some((
        ida_ea,
        json!({
            "input": text,
            "kind": "module_rva",
            "module": module,
            "rva": rva,
            "ida_image_base": MOCK_IDA_IMAGE_BASE,
            "ida_ea": ida_ea,
            "confidence": "medium"
        }),
    )))
}

pub(super) fn mock_ida_ea_normalized_address(input: &Value, ida_ea: u64) -> Value {
    json!({
        "input": input,
        "kind": "ida_ea",
        "ida_ea": ida_ea,
        "confidence": "high"
    })
}

pub(super) fn looks_like_ttd_position(text: &str) -> bool {
    let value = text.trim();
    let value = value.split_once("..").map_or(value, |(start, _)| start);
    let Some((left, right)) = value.split_once(':') else {
        return false;
    };
    !left.is_empty()
        && !right.is_empty()
        && !right.contains(':')
        && left.chars().all(|ch| ch.is_ascii_hexdigit())
        && right.chars().all(|ch| ch.is_ascii_hexdigit())
}

pub(super) fn optional_u64_argument(
    arguments: &Value,
    field: &'static str,
) -> Result<Option<u64>, ServiceError> {
    match arguments.get(field) {
        Some(value) => parse_u64_value(value)
            .map(Some)
            .ok_or_else(|| ServiceError::Rpc(format!("{field} must be an unsigned integer"))),
        None => Ok(None),
    }
}

pub(super) fn parse_u64_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => parse_optional_u64_text(text),
        _ => None,
    }
}

pub(super) fn parse_optional_u64_text(text: &str) -> Option<u64> {
    let had_backtick = text.contains('`');
    let normalized = text.trim().replace('`', "");
    let text = normalized.as_str();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(binary) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        u64::from_str_radix(binary, 2).ok()
    } else if had_backtick {
        u64::from_str_radix(text, 16).ok()
    } else {
        text.parse::<u64>().ok()
    }
}

pub(super) fn parse_core_integer(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(bytes) = text
        .strip_prefix("bytes:")
        .or_else(|| text.strip_prefix("bytes_le:"))
    {
        let mut value = 0u64;
        for (index, byte) in bytes
            .split([' ', ',', '-'])
            .filter(|part| !part.is_empty())
            .take(8)
            .enumerate()
        {
            let byte = u8::from_str_radix(byte.trim_start_matches("0x"), 16).ok()?;
            value |= (byte as u64) << (index * 8);
        }
        Some(value)
    } else if let Some(ascii) = text.strip_prefix("ascii:") {
        let mut value = 0u64;
        for (index, byte) in ascii.as_bytes().iter().take(8).enumerate() {
            value |= (*byte as u64) << (index * 8);
        }
        Some(value)
    } else {
        parse_optional_u64_text(text)
    }
}

pub(super) fn ascii_from_core_integer(value: u64) -> String {
    value
        .to_le_bytes()
        .into_iter()
        .take_while(|byte| *byte != 0)
        .map(|byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                byte as char
            } else {
                '.'
            }
        })
        .collect()
}

pub(super) fn write_mock_eval_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    command: String,
    artifact_dir: PathBuf,
) -> Result<WorkerResponse, ServiceError> {
    let raw_relative_path = session_relative_path(
        &session_id,
        &format!("raw/{}.txt", operation_id.id.as_str()),
    );
    let transcript_relative_path = session_relative_path(&session_id, "transcript.log");
    let events_relative_path = session_relative_path(&session_id, "events.jsonl");
    let raw_path = artifact_dir
        .join("raw")
        .join(format!("{}.txt", operation_id.id.as_str()));
    let transcript_path = artifact_dir.join("transcript.log");
    let events_path = artifact_dir.join("events.jsonl");
    if let Some(parent) = raw_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let output = if command == ".mock_long_output" {
        "x".repeat(DEFAULT_INLINE_RESULT_BYTE_LIMIT + 100)
    } else {
        format!(
            "mock debug worker accepted eval command; real DbgEng execution is not wired yet\ncommand: {}\n",
            command
        )
    };
    write_atomic_bytes(&raw_path, output.as_bytes())?;
    let transcript = format!("> {}\n{}\n", command, output);
    let mut transcript_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&transcript_path)?;
    transcript_file.write_all(transcript.as_bytes())?;
    let mut events_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_path)?;
    serde_json::to_writer(
        &mut events_file,
        &json!({
            "event": "output",
            "session_id": session_id,
            "operation_id": operation_id,
            "command": command,
            "timestamp": Timestamp::now(),
            "byte_len": output.len(),
        }),
    )?;
    events_file.write_all(b"\n")?;
    let preview = ResultPreview::utf8(&output, DEFAULT_INLINE_RESULT_BYTE_LIMIT);

    Ok(WorkerResponse::DebugCommand {
        result: DebugCommandResult {
            session_id,
            operation_id: None,
            command,
            result: preview,
            final_state: Some(DebugSessionState::Ready),
            warnings: vec!["mock worker: real DbgEng execution is not wired yet".to_string()],
            error: None,
        },
        writes: vec![
            WorkerArtifactWrite {
                relative_path: raw_relative_path,
                kind: "debug.raw_output".to_string(),
                role: Some(WorkerArtifactRole::FullResult),
                byte_len: output.len() as u64,
                description: Some("mock debug eval raw output".to_string()),
            },
            WorkerArtifactWrite {
                relative_path: transcript_relative_path,
                kind: "debug.transcript".to_string(),
                role: None,
                byte_len: transcript.len() as u64,
                description: Some("debug session transcript".to_string()),
            },
            WorkerArtifactWrite {
                relative_path: events_relative_path,
                kind: "debug.events".to_string(),
                role: None,
                byte_len: output.len() as u64,
                description: Some("debug session events".to_string()),
            },
        ],
    })
}

pub(super) fn write_mock_memory_response(
    session_id: SessionRef,
    operation_id: OperationRef,
    address: u64,
    length: u64,
    artifact_dir: PathBuf,
) -> Result<WorkerResponse, ServiceError> {
    let length = usize::try_from(length)
        .map_err(|_| ServiceError::Worker("memory read length is too large".to_string()))?;
    let relative_path = session_relative_path(
        &session_id,
        &format!("memory/{}.bin", operation_id.id.as_str()),
    );
    let memory_path = artifact_dir
        .join("memory")
        .join(format!("{}.bin", operation_id.id.as_str()));
    if let Some(parent) = memory_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = vec![0u8; length];
    fs::write(&memory_path, &bytes)?;

    Ok(WorkerResponse::DebugMemory {
        result: DebugMemoryResult {
            session_id,
            operation_id: None,
            address,
            requested_length: length as u64,
            bytes_read: length as u64,
            memory: None,
            warnings: vec!["mock worker: real DbgEng memory read is not wired yet".to_string()],
            error: None,
        },
        writes: vec![WorkerArtifactWrite {
            relative_path,
            kind: "debug.memory".to_string(),
            role: None,
            byte_len: length as u64,
            description: Some("mock debug memory read".to_string()),
        }],
    })
}
