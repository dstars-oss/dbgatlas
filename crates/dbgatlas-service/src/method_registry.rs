use super::DEFAULT_SERVICE_UPDATE_TIMEOUT_MS;
use serde_json::{Value, json};
use std::sync::OnceLock;

static METHOD_SPECS: OnceLock<Vec<MethodSpec>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MethodRoute {
    Service,
    Workspace,
    Session,
    Debug,
    Recording,
    Operation,
    Execution,
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
        .filter(|spec| {
            (!spec.name.starts_with("debug.") || spec.name == "debug.read_memory")
                && !spec.name.starts_with("operation.")
                && !spec.name.starts_with("workspace.")
        })
        .map(|spec| spec.descriptor)
        .collect()
}

fn method_route(name: &str) -> MethodRoute {
    match name.split_once('.').map(|(prefix, _)| prefix) {
        Some("workspace") | Some("history") => MethodRoute::Workspace,
        Some("service") => MethodRoute::Service,
        Some("session") => MethodRoute::Session,
        Some("debug") => MethodRoute::Debug,
        Some("recording") => MethodRoute::Recording,
        Some("operation") => MethodRoute::Operation,
        Some("execution") => MethodRoute::Execution,
        Some("artifact") => MethodRoute::Artifact,
        Some("audit") => MethodRoute::Audit,
        _ => panic!("registered method has unsupported namespace: {name}"),
    }
}

fn registered_method_descriptors() -> Vec<Value> {
    let mut tools = vec![
        mcp_tool(
            "history.query",
            "Browse the central catalog: overview lists projects; workspace pages executions; history reads changes; provenance finds file producers and consumers; output reads registered files.",
            json!({"type":"object","properties":{
                "action":{"enum":["overview","workspace","history","provenance","output","facts"]},"project_root":{"type":"string"},"workspace_root":{"type":"string"},
                "execution_id":{"type":"string"},"session_id":{"type":"string"},"artifact_id":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]},
                "status":{"type":"string"},"search":{"type":"string"},"start_ms":{"type":"integer"},"end_ms":{"type":"integer"},"after":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"}
            }}),
        ),
        mcp_tool(
            "workspace.init",
            "Register a new workspace.",
            json!({"type":"object","properties":{"path":{"type":"string"},"with_inputs":{"type":"boolean"}},"required":["path"]}),
        ),
        mcp_tool(
            "workspace.relink",
            "Bind a moved workspace to its registered identity.",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        ),
        mcp_tool(
            "workspace.info",
            "Read registered workspace information.",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        ),
        mcp_tool(
            "session.interrupt",
            "Request a break for the observed target run through the independent control channel. Accepted is not a confirmed stop; query session.get. A stale epoch never interrupts a later run.",
            json!({"type":"object","properties":{"session_id":session_ref_schema(),"expected_run_epoch":{"type":"integer","minimum":1}},"required":["session_id"],"additionalProperties":false}),
        ),
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
            "session.get",
            "Get a session summary, including terminal sessions from this service instance.",
            mcp_session_schema(),
        ),
        mcp_tool(
            "session.list",
            "List sessions; defaults to active debug sessions.",
            json!({ "type": "object", "properties": {
                "kind": {"type":"string", "enum":["debug","recording","all"], "default":"debug"},
                "include_terminal":{"type":"boolean", "default":false}
            } }),
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
                        "description": "Rejected legacy execution deadline. Use wait_ms for bounded observation."
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
                        "description": "Rejected legacy per-step deadline. Use wait_ms for the whole-call observation budget."
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
    tools.extend(execution_descriptors());
    for tool in &mut tools {
        let name = tool["name"].as_str().unwrap_or("").to_string();
        if matches!(name.as_str(), "session.get" | "session.interrupt") {
            tool["inputSchema"]["properties"]["wait_ms"] = json!({"type":"integer","minimum":0,"maximum":30000,"default":if name=="session.interrupt" {1000} else {0}});
            if name == "session.get" {
                tool["inputSchema"]["properties"]["after_seq"] =
                    json!({"type":"integer","minimum":0});
            } else {
                tool["inputSchema"]["properties"]["expected_run_epoch"] =
                    json!({"type":"integer","minimum":1});
            }
        }
        if name == "debug.read_memory" {
            tool["inputSchema"]["properties"]["wait_ms"] =
                json!({"type":"integer","minimum":0,"maximum":30000,"default":1000});
        }
        if name == "operation.stream" {
            tool["description"] = json!(
                "Compatibility alias for execution.read: bounded output items, next_cursor, has_more, complete and gap."
            );
            tool["inputSchema"]["properties"]["cursor"] = json!({"type":"string"});
            tool["inputSchema"]["properties"]["max_bytes"] =
                json!({"type":"integer","minimum":4,"maximum":65536,"default":8192});
        }
        if matches!(name.as_str(), "debug.eval" | "debug.eval_steps") {
            tool["inputSchema"]["properties"]["timeout_ms"] = json!({"type":"integer","minimum":0,"maximum":30000,"description":"Rejected legacy execution deadline. Use wait_ms for bounded observation; cancel or kill must be explicit."});
            tool["inputSchema"]["properties"]["wait_ms"] =
                json!({"type":"integer","minimum":0,"maximum":30000,"default":1000});
        }
    }
    tools
}

fn execution_descriptors() -> Vec<Value> {
    let wait = json!({"type":"integer","minimum":0,"maximum":30000,"default":1000});
    let reference = session_ref_schema();
    let mut target = debug_target_schema();
    target["oneOf"][0]["properties"]["artifact_ref"] =
        ref_id_schema("Artifact reference in this workspace", "artifact-1");
    target["oneOf"][0]["required"] = json!(["kind"]);
    target["oneOf"][0]["oneOf"] = json!([{"required":["path"],"not":{"required":["artifact_ref"]}},{"required":["artifact_ref"],"not":{"required":["path"]}}]);
    let schema = |properties: Value, required: Value| json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    let mut tools = vec![
        mcp_tool(
            "session.open",
            "Open a dbgeng session. The service owns opening after the wait budget expires; use the returned session/execution IDs to observe it. Do not automatically replay a launch after losing its response.",
            schema(
                json!({"project_root":{"type":"string"},"backend":{"enum":["dbgeng"],"default":"dbgeng"},"target":target,"worker_identity":{"enum":["default","active_interactive_user"]},"startup_timeout_ms":{"type":"integer","minimum":1},"wait_ms":wait}),
                json!(["project_root", "target"]),
            ),
        ),
        mcp_tool(
            "session.exec",
            "Execute one native command. wait_ms only bounds observation; it never cancels execution. Busy sessions reject new commands. Retry with the same client_request_id and expected_service_instance to recover the same execution.",
            schema(
                json!({"session_id":reference,"command":{"type":"string","minLength":1},"wait_ms":wait,"client_request_id":{"type":"string","minLength":1,"maxLength":256},"expected_service_instance":{"type":"string"}}),
                json!(["session_id", "command"]),
            ),
        ),
        mcp_tool(
            "execution.get",
            "Observe one execution without repeating its output. after_seq waits for a later update; output is read through execution.read.",
            schema(
                json!({"execution_id":operation_ref_schema(),"after_seq":{"type":"integer","minimum":0},"wait_ms":{"type":"integer","minimum":0,"maximum":30000,"default":0}}),
                json!(["execution_id"]),
            ),
        ),
        mcp_tool(
            "execution.list",
            "Page through recorded executions, including closed sessions. Historical unfinished records are unknown, not live executions.",
            schema(
                json!({"project_root":{"type":"string"},"session_id":reference,"status":{"enum":["pending","running","success","failed","canceled","unknown"]},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}}),
                json!(["project_root"]),
            ),
        ),
        mcp_tool(
            "execution.cancel",
            "Request cooperative cancellation of this execution. Returns a separate control execution; acceptance does not prove the original was canceled. Completed executions return already_finished.",
            schema(
                json!({"execution_id":operation_ref_schema(),"wait_ms":wait}),
                json!(["execution_id"]),
            ),
        ),
        mcp_tool(
            "session.close",
            "Close cooperatively and preserve the target. A short wait can return closing; repeated close returns the same execution. Use explicit kill if the backend cannot cooperate.",
            schema(
                json!({"session_id":reference,"wait_ms":wait}),
                json!(["session_id"]),
            ),
        ),
        mcp_tool(
            "session.kill",
            "Force cleanup of the dedicated worker and verified owned resources. Attached targets are preserved; the session becomes failed, with cleanup reported separately.",
            schema(
                json!({"session_id":reference,"wait_ms":wait}),
                json!(["session_id"]),
            ),
        ),
        mcp_tool(
            "session.help",
            "Get focused native execution, observation, output and control guidance for a session or backend.",
            schema(
                json!({"session_id":reference,"backend":{"enum":["dbgeng"]},"topic":{"enum":["overview","execution","output","control"],"default":"overview"}}),
                json!([]),
            ),
        ),
    ];
    for (name, id, description) in [
        (
            "execution.read",
            "execution_id",
            "Read bounded registered execution text and artifact references, including while running or after session close.",
        ),
        (
            "session.read",
            "session_id",
            "Read bounded asynchronous session output and event JSONL. This does not repeat individual command output.",
        ),
    ] {
        let mut properties = json!({"cursor":{"type":"string"},"max_bytes":{"type":"integer","minimum":4,"maximum":65536,"default":8192}});
        properties[id] = reference.clone();
        tools.push(mcp_tool(name, description, schema(properties, json!([id]))));
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
                    "pid": { "type": "integer", "minimum": 1 },
                    "mode":{"type":"string","enum":["noninvasive","invasive"],"default":"noninvasive","description":"Explicit invasive attach enables target control; the default observes without taking control."}
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
