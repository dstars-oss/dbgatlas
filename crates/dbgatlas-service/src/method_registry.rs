use super::DEFAULT_SERVICE_UPDATE_TIMEOUT_MS;
use serde_json::{Value, json};
use std::sync::OnceLock;

static METHOD_SPECS: OnceLock<Vec<MethodSpec>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MethodRoute {
    Service,
    Session,
    Debug,
    Recording,
    Operation,
    Artifact,
    Audit,
}

#[derive(Clone, Debug)]
pub(super) struct MethodSpec {
    pub(super) name: String,
    pub(super) route: MethodRoute,
    pub(super) descriptor: Value,
}

pub(super) fn method_spec(name: &str) -> Option<MethodSpec> {
    registered_method_specs()
        .iter()
        .find(|spec| spec.name == name)
        .cloned()
}

pub(super) fn method_specs() -> Vec<MethodSpec> {
    registered_method_specs().to_vec()
}

fn registered_method_specs() -> &'static [MethodSpec] {
    METHOD_SPECS.get_or_init(|| {
        registered_method_descriptors()
            .into_iter()
            .map(|descriptor| {
                let name = descriptor["name"]
                    .as_str()
                    .expect("registered method name is a string")
                    .to_string();
                MethodSpec {
                    route: method_route(&name),
                    name,
                    descriptor,
                }
            })
            .collect()
    })
}

pub(super) fn mcp_tool_descriptors() -> Vec<Value> {
    method_specs()
        .into_iter()
        .map(|spec| spec.descriptor)
        .collect()
}

fn method_route(name: &str) -> MethodRoute {
    match name.split_once('.').map(|(prefix, _)| prefix) {
        Some("service") => MethodRoute::Service,
        Some("session") => MethodRoute::Session,
        Some("debug") => MethodRoute::Debug,
        Some("recording") => MethodRoute::Recording,
        Some("operation") => MethodRoute::Operation,
        Some("artifact") => MethodRoute::Artifact,
        Some("audit") => MethodRoute::Audit,
        _ => panic!("registered method has unsupported namespace: {name}"),
    }
}

fn registered_method_descriptors() -> Vec<Value> {
    let tools = vec![
        mcp_tool(
            "service.health",
            "Return DbgAtlas service health.",
            empty_object_schema(),
        ),
        mcp_tool(
            "service.info",
            "Return DbgAtlas service information.",
            empty_object_schema(),
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
            "List current debug sessions known to this service process.",
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
            "recording.start",
            "Start an ETW process-tree recording and return its recording and operation refs.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "target": etw_recording_target_schema(),
                    "presets": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["process", "thread", "image", "file", "registry", "network", "cpu"]
                        }
                    },
                    "cpu_sampling": {
                        "type": "object",
                        "properties": {
                            "interval_us": { "type": "integer", "minimum": 125, "maximum": 1000000, "default": 1000 },
                            "max_trace_size_mb": { "type": "integer", "minimum": 64, "maximum": 4096, "default": 512 }
                        },
                        "additionalProperties": false
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
            "recording.status",
            "Return the current state and artifact refs for a recording.",
            mcp_recording_schema(),
        ),
        mcp_tool(
            "recording.stop",
            "Stop a running recording and finalize its trace and event artifacts.",
            mcp_recording_schema(),
        ),
        mcp_tool(
            "recording.cancel",
            "Cancel a running recording while preserving its audit trail.",
            mcp_recording_schema(),
        ),
        mcp_tool(
            "recording.kill",
            "Forcefully terminate a recording worker and preserve failure artifacts.",
            mcp_recording_schema(),
        ),
        mcp_tool(
            "artifact.list",
            "List persisted artifact metadata for a project analysis workspace.",
            project_root_schema(),
        ),
        mcp_tool(
            "operation.list",
            "List persisted operation records for a project analysis workspace.",
            project_root_schema(),
        ),
        mcp_tool(
            "audit.list_commands",
            "List persisted command audit records for a project analysis workspace.",
            project_root_schema(),
        ),
    ];
    tools
}

fn mcp_tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn empty_object_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

fn project_root_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "project_root": { "type": "string" }
        },
        "required": ["project_root"]
    })
}

fn etw_recording_target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "type": { "const": "launch" },
                    "executable": { "type": "string" },
                    "args": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["type", "executable", "args"]
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "attach" },
                    "pid": { "type": "integer", "minimum": 1 }
                },
                "required": ["type", "pid"]
            }
        ]
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
                    "start_directory": {
                        "type": "string",
                        "description": "Optional working directory used when launching TTD.exe for this launch target. This is not the trace output directory."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "default": []
                    }
                },
                "additionalProperties": false,
                "required": ["kind", "executable"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "attach" },
                    "pid": { "type": "integer", "minimum": 1 }
                },
                "additionalProperties": false,
                "required": ["kind", "pid"]
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "monitor" },
                    "program": { "type": "string" },
                    "cmd_line_filter": { "type": "string" }
                },
                "additionalProperties": false,
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

fn recording_ref_schema() -> Value {
    ref_id_schema("recording", "recording-123")
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

fn mcp_operation_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation_id": operation_ref_schema()
        },
        "required": ["operation_id"]
    })
}

fn mcp_recording_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "recording_id": recording_ref_schema()
        },
        "required": ["recording_id"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registered_methods_are_unique_and_have_object_schemas() {
        let specs = method_specs();
        let mut names = HashSet::new();
        for spec in &specs {
            assert!(names.insert(spec.name.clone()), "duplicate {}", spec.name);
            assert_eq!(spec.descriptor["inputSchema"]["type"], "object");
            assert_eq!(method_spec(&spec.name).unwrap().route, spec.route);
        }
    }

    #[test]
    fn canonical_inventory_excludes_retired_methods() {
        let names = method_specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<HashSet<_>>();
        assert!(names.iter().all(|name| !name.starts_with("reverse.")));
        assert!(method_spec("reverse.py_eval").is_none());
        assert!(!names.contains("workspace.facts"));
        for name in [
            "artifact.list",
            "operation.list",
            "audit.list_commands",
            "recording.start",
            "recording.status",
            "recording.stop",
            "recording.cancel",
            "recording.kill",
        ] {
            assert!(names.contains(name), "missing {name}");
        }
    }
}
