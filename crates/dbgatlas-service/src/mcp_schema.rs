use super::{DEFAULT_SERVICE_UPDATE_TIMEOUT_MS, ServiceCapabilities};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum McpToolRoute {
    ServiceRpc,
    WorkspaceFacts,
    ReversePyEval,
}

pub(super) fn mcp_tool_route(name: &str) -> Option<McpToolRoute> {
    match name {
        "workspace.facts" => Some(McpToolRoute::WorkspaceFacts),
        "reverse.py_eval" => Some(McpToolRoute::ReversePyEval),
        _ if mcp_tool_names_include(name) => Some(McpToolRoute::ServiceRpc),
        _ => None,
    }
}

fn mcp_tool_names_include(name: &str) -> bool {
    mcp_tool_descriptors(ServiceCapabilities {
        ida_py_eval: true,
        ..ServiceCapabilities::default()
    })
    .iter()
    .any(|tool| tool["name"] == name)
}

pub(super) fn mcp_tool_descriptors(capabilities: ServiceCapabilities) -> Vec<Value> {
    let mut tools = vec![
        mcp_tool(
            "service.health",
            "Return DbgAtlas service health.",
            json!({}),
        ),
        mcp_tool(
            "service.info",
            "Return DbgAtlas service information.",
            json!({}),
        ),
        mcp_tool(
            "service.update",
            "Update the installed DbgAtlas service from a built payload directory.",
            json!({
                "type": "object",
                "properties": {
                    "source_dir": { "type": "string" },
                    "restart": { "type": "boolean", "default": true },
                    "timeout_ms": { "type": "integer", "default": DEFAULT_SERVICE_UPDATE_TIMEOUT_MS }
                },
                "required": ["source_dir"]
            }),
        ),
        mcp_tool(
            "session.list",
            "List current debug and reverse sessions known to this service process.",
            json!({ "type": "object", "properties": {} }),
        ),
        mcp_tool(
            "debug.session.create",
            "Create a debug session from a file, attach, or launch target. Use kind=file with a .run path for TTD replay.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "target": debug_target_schema(),
                    "startup_timeout_ms": { "type": "integer" },
                    "worker_identity": {
                        "type": "string",
                        "enum": ["default", "active_interactive_user"],
                        "default": "default",
                        "description": "Use active_interactive_user for user-session TTD replay or live launch when the installed service default LocalSystem identity cannot access the target."
                    }
                },
                "required": ["project_root", "target"]
            }),
        ),
        mcp_tool(
            "recording.ttd",
            "Record a Time Travel Debugging trace with TTD.exe and register the .run/.idx artifacts. To replay an existing .run trace, use debug.session.create with target kind=file.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "target": ttd_recording_target_schema(),
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Maximum recording duration in milliseconds before DbgAtlas asks TTD.exe to stop and keeps any completed trace artifacts."
                    },
                    "worker_identity": {
                        "type": "string",
                        "enum": ["default", "active_interactive_user"],
                        "default": "default",
                        "description": "Run TTD.exe as the service process by default. Use active_interactive_user when an installed service must attach to a process in the current interactive user session."
                    },
                    "options": ttd_recording_options_schema()
                },
                "required": ["project_root", "target", "timeout_ms"]
            }),
        ),
        mcp_tool(
            "debug.eval",
            "Execute one raw DbgEng IDebugControl::Execute command string in an existing session. This does not emulate WinDbg command-window multiline input; use debug.eval_steps for ordered multi-step commands.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "command": {
                        "type": "string",
                        "description": "Raw DbgEng command string sent as a single IDebugControl::Execute call. Newlines are not treated as separate WinDbg command-window submissions; use debug.eval_steps when commands such as .exepath/.sympath must be isolated from following commands."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Optional command timeout in milliseconds. On timeout DbgAtlas kills the debug worker and the session must be recreated."
                    }
                },
                "required": ["session_id", "command"]
            }),
        ),
        mcp_tool(
            "debug.eval_steps",
            "Execute raw DbgEng commands one step at a time in an existing session, with each array item sent as a separate IDebugControl::Execute call.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "commands": {
                        "type": "array",
                        "description": "Ordered command steps. Each item is executed separately, so line-consuming commands such as .exepath/.sympath do not consume later steps.",
                        "items": { "type": "string" },
                        "minItems": 1
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Optional timeout per step in milliseconds. On timeout DbgAtlas kills the debug worker and the session must be recreated."
                    },
                    "continue_on_error": {
                        "type": "boolean",
                        "default": false
                    }
                },
                "required": ["session_id", "commands"]
            }),
        ),
        mcp_tool(
            "debug.modules",
            "List modules for a debug session. In TTD replay, modules that were loaded and unloaded outside the current position may require targeted TTD calls or memory evidence.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.threads",
            "List threads for a debug session.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.stack",
            "Get stack for a debug session.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.add_symbols",
            "Append a symbol path to a debug session using .sympath+. Set reload=true to run .reload after updating the path.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "symbol_path": {
                        "type": "string",
                        "description": "Symbol path segment to append with .sympath+; this does not replace the existing symbol path."
                    },
                    "reload": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, execute .reload after appending the symbol path."
                    }
                },
                "required": ["session_id", "symbol_path"]
            }),
        ),
        mcp_tool(
            "debug.read_memory",
            "Read virtual memory to an artifact.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "address": {},
                    "length": { "type": "integer" }
                },
                "required": ["session_id", "address", "length"]
            }),
        ),
        mcp_tool(
            "reverse.session.open",
            "Open an IDA reverse session for an IDB/database path. This is not a read-only clone; opening an IDB already held by another IDA instance can fail with lock/availability diagnostics.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "database_path": {
                        "type": "string",
                        "description": "Path to the IDA database or input file to open in the session."
                    },
                    "ida_install_dir": {
                        "type": "string",
                        "description": "Optional IDA installation directory override."
                    }
                },
                "required": ["project_root", "database_path"]
            }),
        ),
        mcp_tool(
            "reverse.lookup_function",
            "Map a runtime address to an IDA function and record the lookup.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema(),
                    "runtime_address": {},
                    "runtime_module_base": {},
                    "ida_image_base": {}
                },
                "required": [
                    "session_id",
                    "runtime_address",
                    "runtime_module_base",
                    "ida_image_base"
                ]
            }),
        ),
        mcp_tool(
            "reverse.lookup_funcs",
            "Lookup IDA functions by address, module+RVA, or name.",
            mcp_reverse_core_schema_required(
                json!({
                "queries": {
                    "description": "Address or name queries. Address strings may be IDA EA values such as 0x180093660 or module+RVA values such as libxsse+0x93660."
                },
                "runtime_module_base": {},
                "ida_image_base": {}
                }),
                &["queries"],
            ),
        ),
        mcp_tool(
            "reverse.int_convert",
            "Convert decimal, hex, bytes, ASCII, and binary integer representations.",
            mcp_reverse_core_schema_required(json!({ "inputs": {} }), &["inputs"]),
        ),
        mcp_tool(
            "reverse.list_funcs",
            "List IDA functions with pagination and optional filtering.",
            mcp_reverse_core_schema(json!({
                "offset": { "type": "integer" },
                "count": { "type": "integer" },
                "filter": { "type": "string" }
            })),
        ),
        mcp_tool(
            "reverse.list_globals",
            "List IDA global variables with pagination and optional filtering.",
            mcp_reverse_core_schema(json!({
                "offset": { "type": "integer" },
                "count": { "type": "integer" },
                "filter": { "type": "string" }
            })),
        ),
        mcp_tool(
            "reverse.list_imports",
            "List imported symbols and module names with pagination.",
            mcp_reverse_core_schema(json!({
                "offset": { "type": "integer" },
                "count": { "type": "integer" },
                "filter": { "type": "string" }
            })),
        ),
        mcp_tool(
            "reverse.list_strings",
            "List IDA strings with pagination and optional substring filtering.",
            mcp_reverse_core_schema(json!({
                "offset": { "type": "integer" },
                "count": { "type": "integer" },
                "filter": { "type": "string" }
            })),
        ),
        mcp_tool(
            "reverse.get_string",
            "Read an IDA string at an address.",
            mcp_reverse_core_schema_required(
                json!({
                    "addr": reverse_address_schema(),
                    "length": { "type": "integer" },
                    "type": { "type": "integer" }
                }),
                &["addr"],
            ),
        ),
        mcp_tool(
            "reverse.get_bytes",
            "Read IDB bytes at an address.",
            mcp_reverse_core_schema_required(
                json!({
                    "addr": reverse_address_schema(),
                    "length": { "type": "integer" }
                }),
                &["addr", "length"],
            ),
        ),
        mcp_tool(
            "reverse.get_int",
            "Read an integer from IDB bytes at an address.",
            mcp_reverse_core_schema_required(
                json!({
                    "addr": reverse_address_schema(),
                    "size": { "type": "integer" },
                    "endian": {
                        "type": "string",
                        "enum": ["little", "big"],
                        "default": "little"
                    }
                }),
                &["addr"],
            ),
        ),
        mcp_tool(
            "reverse.decompile",
            "Decompile the function containing an IDA address.",
            mcp_reverse_core_schema_required(
                json!({ "addr": reverse_address_schema() }),
                &["addr"],
            ),
        ),
        mcp_tool(
            "reverse.disasm",
            "Disassemble the function containing an IDA address.",
            mcp_reverse_core_schema_required(
                json!({ "addr": reverse_address_schema() }),
                &["addr"],
            ),
        ),
        mcp_tool(
            "reverse.xrefs_to",
            "Find cross-references to one or more IDA addresses.",
            mcp_reverse_core_schema_required(
                json!({
                    "addrs": {
                        "description": "One or more IDA EA or module+RVA code addresses."
                    }
                }),
                &["addrs"],
            ),
        ),
        mcp_tool(
            "reverse.xrefs_to_field",
            "Find cross-references to struct fields.",
            mcp_reverse_core_schema_required(json!({ "queries": {} }), &["queries"]),
        ),
        mcp_tool(
            "reverse.callees",
            "List callees for one or more IDA functions.",
            mcp_reverse_core_schema_required(
                json!({
                    "addrs": {
                        "description": "One or more IDA EA or module+RVA code addresses."
                    }
                }),
                &["addrs"],
            ),
        ),
        mcp_tool(
            "reverse.rename",
            "Rename IDA functions or globals in the current session. This mutates the IDB; call reverse.idb_save to persist changes when needed.",
            mcp_reverse_core_schema_required(
                json!({ "items": reverse_rename_items_schema() }),
                &["items"],
            ),
        ),
        mcp_tool(
            "reverse.set_comments",
            "Set IDA comments in the current session. This mutates the IDB; call reverse.idb_save to persist changes when needed.",
            mcp_reverse_core_schema_required(
                json!({ "items": reverse_set_comments_items_schema() }),
                &["items"],
            ),
        ),
        mcp_tool(
            "reverse.set_type",
            "Apply C types to IDA functions, globals, or addresses in the current session. This mutates the IDB; call reverse.idb_save to persist changes when needed.",
            mcp_reverse_core_schema_required(
                json!({ "items": reverse_set_type_items_schema() }),
                &["items"],
            ),
        ),
        mcp_tool(
            "reverse.declare_type",
            "Declare C types in the IDA local type library. This mutates the IDB; call reverse.idb_save to persist changes when needed.",
            mcp_reverse_core_schema_required(json!({ "decls": {} }), &["decls"]),
        ),
        mcp_tool(
            "reverse.inspect_item",
            "Inspect IDA item boundaries, head address, current name, and data/code state without mutating the IDB.",
            mcp_reverse_core_schema_required(json!({ "queries": {} }), &["queries"]),
        ),
        mcp_tool(
            "reverse.force_recompile",
            "Invalidate Hex-Rays cached decompilation for functions or all functions in the current session.",
            mcp_reverse_core_schema(json!({ "addrs": {} })),
        ),
        mcp_tool(
            "reverse.idb_save",
            "Save the current IDA database. When path is omitted, save in place.",
            mcp_reverse_core_schema(json!({
                "path": {
                    "type": "string",
                    "description": "Optional output path. Omit to save the currently opened database in place."
                }
            })),
        ),
        mcp_tool(
            "reverse.find_bytes",
            "Find byte patterns in the IDA database.",
            mcp_reverse_core_schema_required(
                json!({
                    "patterns": {},
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                }),
                &["patterns"],
            ),
        ),
        mcp_tool(
            "reverse.search_text",
            "Search IDA strings, names, disassembly, and comments by substring.",
            mcp_reverse_core_schema_required(
                json!({
                    "query": { "type": "string" },
                    "scope": {
                        "type": "string",
                        "enum": ["strings", "names", "disasm", "comments", "all"]
                    },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                }),
                &["query"],
            ),
        ),
        mcp_tool(
            "reverse.query_xrefs",
            "Query cross-references to or from an address or name.",
            mcp_reverse_core_schema_required(
                json!({
                    "target": {},
                    "direction": { "type": "string", "enum": ["to", "from"] },
                    "xref_type": { "type": "string", "enum": ["code", "data", "all"] },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                }),
                &["target"],
            ),
        ),
        mcp_tool(
            "reverse.query_funcs",
            "Query IDA functions with richer filtering and sorting.",
            mcp_reverse_core_schema(json!({
                "filter": { "type": "string" },
                "name_regex": { "type": "string" },
                "min_size": { "type": "integer" },
                "max_size": { "type": "integer" },
                "has_type": { "type": "boolean" },
                "sort_by": { "type": "string", "enum": ["addr", "name", "size"] },
                "descending": { "type": "boolean" },
                "offset": { "type": "integer" },
                "count": { "type": "integer" }
            })),
        ),
        mcp_tool(
            "reverse.query_entities",
            "Query IDB entities with filtering and pagination.",
            mcp_reverse_core_schema_required(
                json!({
                    "kind": {
                        "type": "string",
                        "enum": ["functions", "globals", "imports", "strings", "names"]
                    },
                    "filter": { "type": "string" },
                    "fields": {},
                    "offset": { "type": "integer" },
                    "count": { "type": "integer" }
                }),
                &["kind"],
            ),
        ),
        mcp_tool(
            "reverse.session.close",
            "Close an IDA reverse session.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": session_ref_schema()
                },
                "required": ["session_id"]
            }),
        ),
        mcp_tool(
            "debug.session.close",
            "Cooperatively close a debug session. If the worker is stuck or the pipe is broken, use debug.session.kill.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "debug.session.kill",
            "Forcefully terminate a debug session worker and mark the session closed.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "operation.get",
            "Return an operation status and artifact refs.",
            mcp_operation_schema(),
        ),
        mcp_tool(
            "operation.cancel",
            "Cancel a running operation.",
            mcp_operation_schema(),
        ),
        mcp_tool(
            "operation.stream",
            "Return operation events.",
            mcp_operation_schema(),
        ),
        mcp_tool(
            "workspace.facts",
            "Read workspace facts: artifact registry, operations, and command audit.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        ),
    ];
    if capabilities.ida_py_eval {
        let py_eval = mcp_tool(
            "reverse.py_eval",
            "Execute Python code in the IDA context.",
            mcp_reverse_core_schema_required(json!({ "code": { "type": "string" } }), &["code"]),
        );
        let insert_at = tools
            .iter()
            .position(|tool| tool["name"] == "reverse.find_bytes")
            .unwrap_or(tools.len());
        tools.insert(insert_at, py_eval);
    }
    tools
}

fn mcp_tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn debug_target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "file" },
                    "path": {
                        "type": "string",
                        "description": "Path to a dump or TTD .run trace. TTD replay uses kind=file, not kind=ttd."
                    }
                },
                "required": ["kind", "path"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "attach" },
                    "pid": { "type": "integer", "minimum": 1 }
                },
                "required": ["kind", "pid"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "launch" },
                    "executable": { "type": "string" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "default": []
                    }
                },
                "required": ["kind", "executable"]
            }
        ]
    })
}

fn ttd_recording_target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "launch" },
                    "executable": { "type": "string" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "default": []
                    }
                },
                "required": ["kind", "executable"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "attach" },
                    "pid": { "type": "integer", "minimum": 1 }
                },
                "required": ["kind", "pid"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "monitor" },
                    "program": { "type": "string" },
                    "cmd_line_filter": { "type": "string" }
                },
                "required": ["kind", "program"]
            }
        ]
    })
}

fn ttd_recording_options_schema() -> Value {
    json!({
        "type": "object",
        "description": "TTD.exe recording options. Defaults: no_ui=true, max_file_mb=2048, accept_eula=false, children=false, ring=false, modules=[], record_mode=automatic, replay_cpu_support=default.",
        "properties": {
            "children": {
                "type": "boolean",
                "default": false,
                "description": "Pass -children so child processes are recorded too."
            },
            "no_ui": {
                "type": "boolean",
                "default": true,
                "description": "Pass -noUI to suppress the TTD recorder UI."
            },
            "accept_eula": {
                "type": "boolean",
                "default": false,
                "description": "Pass -accepteula to TTD.exe when the caller has already accepted the TTD license terms."
            },
            "ring": {
                "type": "boolean",
                "default": false,
                "description": "Pass -ring to record in ring-buffer mode."
            },
            "max_file_mb": {
                "type": "integer",
                "default": 2048,
                "minimum": 1,
                "description": "Pass -maxFile. Ring mode is limited to 32768 MB; non-ring mode is limited to 1048576 MB."
            },
            "modules": {
                "type": "array",
                "items": { "type": "string" },
                "default": [],
                "description": "Module filters passed as repeated -module arguments. Non-monitor targets allow at most 64 modules."
            },
            "record_mode": {
                "type": "string",
                "enum": ["automatic", "manual"],
                "default": "automatic",
                "description": "TTD -recordmode value."
            },
            "replay_cpu_support": {
                "type": "string",
                "enum": [
                    "default",
                    "most_conservative",
                    "most_aggressive",
                    "intel_avx_required",
                    "intel_avx2_required"
                ],
                "default": "default",
                "description": "TTD -replayCpuSupport value."
            }
        }
    })
}

fn reverse_rename_items_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "description": "Each item must include addr or name to identify the target.",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["function", "global"],
                    "description": "Use function to normalize addr/name to the containing function start; use global for the exact named/addressed item."
                },
                "addr": {
                    "description": "IDA address to rename. Provide addr or name."
                },
                "name": {
                    "type": "string",
                    "description": "Existing IDA name to rename. Provide addr or name."
                },
                "new_name": { "type": "string" }
            },
            "required": ["kind", "new_name"]
        }
    })
}

fn reverse_set_comments_items_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "description": "Each item must include addr or name to identify the comment location.",
            "properties": {
                "addr": {
                    "description": "IDA address to comment. Provide addr or name."
                },
                "name": {
                    "type": "string",
                    "description": "Existing IDA name whose address should receive the comment. Provide addr or name."
                },
                "text": { "type": "string" },
                "repeatable": { "type": "boolean", "default": false }
            },
            "required": ["text"]
        }
    })
}

fn reverse_set_type_items_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "description": "Each item must include addr or name to identify where the type is applied.",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["function", "global", "addr"],
                    "description": "Use function to apply a function declaration at the function start; global or addr applies at the resolved address."
                },
                "addr": {
                    "description": "IDA address to type. Provide addr or name."
                },
                "name": {
                    "type": "string",
                    "description": "Existing IDA name to type. Provide addr or name."
                },
                "type": { "type": "string" }
            },
            "required": ["kind", "type"]
        }
    })
}

fn ref_id_schema(label: &str, example: &str) -> Value {
    json!({
        "oneOf": [
            {
                "type": "string",
                "description": format!("{label} id string, for example \"{example}\".")
            },
            {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"],
                "additionalProperties": true,
                "description": format!("{label} ref object, for example {{\"id\":\"{example}\"}}.")
            }
        ],
        "description": format!(
            "Accepts either a raw {label} id string such as \"{example}\" or a ref object with an id field."
        )
    })
}

fn session_ref_schema() -> Value {
    ref_id_schema("session", "session-123")
}

fn operation_ref_schema() -> Value {
    ref_id_schema("operation", "op-123")
}

fn mcp_session_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session_id": session_ref_schema()
        },
        "required": ["session_id"]
    })
}

fn mcp_reverse_core_schema(extra_properties: Value) -> Value {
    mcp_reverse_core_schema_required(extra_properties, &[])
}

fn mcp_reverse_core_schema_required(extra_properties: Value, extra_required: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("session_id".to_string(), session_ref_schema());
    if let Value::Object(extra) = extra_properties {
        for (key, value) in extra {
            properties.insert(key, value);
        }
    }
    let mut required = vec![json!("session_id")];
    required.extend(extra_required.iter().map(|field| json!(field)));
    json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

fn reverse_address_schema() -> Value {
    json!({
        "description": "IDA EA, decimal integer, WinDbg-style hex integer, or module+RVA string such as libxsse+0x93660. TTD positions such as 151E:2020 are rejected because they are not code addresses."
    })
}

fn mcp_operation_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation_id": operation_ref_schema()
        },
        "required": ["operation_id"]
    })
}
