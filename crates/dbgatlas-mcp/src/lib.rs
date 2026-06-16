use dbgatlas_service::{JsonRpcRequest, JsonRpcResponse, ServiceError, ServiceHost};
use dbgatlas_workspace::{Workspace, WorkspaceError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("unknown MCP method `{0}`")]
    UnknownMethod(String),
    #[error("unknown DbgAtlas tool `{0}`")]
    UnknownTool(String),
    #[error("tool arguments must be a JSON object")]
    InvalidToolArguments,
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub struct McpServer {
    host: ServiceHost,
}

impl McpServer {
    pub fn with_host(host: ServiceHost) -> Self {
        Self { host }
    }

    pub fn with_process_workers() -> Result<Self, McpError> {
        Ok(Self {
            host: ServiceHost::with_process_workers()?,
        })
    }

    pub fn handle_request(&self, request: McpRequest) -> Option<McpResponse> {
        if request.id.is_none() {
            let _ = self.handle_method(request);
            return None;
        }
        let id = request.id.clone();
        Some(match self.handle_method(request) {
            Ok(result) => McpResponse::result(id, result),
            Err(error) => McpResponse::error(id, mcp_error_for(error)),
        })
    }

    pub fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        Ok(self.call_tool_output(name, arguments)?.value)
    }

    fn call_tool_output(&self, name: &str, arguments: Value) -> Result<ToolCallOutput, McpError> {
        match name {
            "service.health"
            | "service.info"
            | "operation.get"
            | "operation.cancel"
            | "operation.stream"
            | "debug.session.create"
            | "debug.session.close"
            | "debug.session.kill"
            | "debug.eval"
            | "debug.modules"
            | "debug.threads"
            | "debug.stack"
            | "debug.add_symbols"
            | "debug.read_memory" => self.call_service_tool(name, arguments),
            "workspace.facts" => Ok(ToolCallOutput::success(self.workspace_facts(arguments)?)),
            other => Err(McpError::UnknownTool(other.to_string())),
        }
    }

    fn handle_method(&self, request: McpRequest) -> Result<Value, McpError> {
        match request.method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "dbgatlas-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "tools": {},
                },
            })),
            "notifications/initialized" => Ok(json!(null)),
            "tools/list" => Ok(json!({ "tools": tool_descriptors() })),
            "tools/call" => {
                let params: ToolCallParams =
                    serde_json::from_value(request.params.unwrap_or_else(|| json!({})))?;
                let result = self.call_tool_output(
                    &params.name,
                    params.arguments.unwrap_or_else(|| json!({})),
                )?;
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result.value)?,
                    }],
                    "isError": result.is_error,
                }))
            }
            other => Err(McpError::UnknownMethod(other.to_string())),
        }
    }

    fn call_service_tool(
        &self,
        method: &str,
        arguments: Value,
    ) -> Result<ToolCallOutput, McpError> {
        if !arguments.is_object() {
            return Err(McpError::InvalidToolArguments);
        }
        let response = self.host.handle_rpc(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params: Some(arguments),
        });
        service_response_result(response)
    }

    fn workspace_facts(&self, arguments: Value) -> Result<Value, McpError> {
        let params: WorkspaceFactsParams = serde_json::from_value(arguments)?;
        let workspace = Workspace::open(params.path)?;
        Ok(serde_json::to_value(workspace.facts()?)?)
    }
}

pub fn serve_stdio<R, W>(server: McpServer, input: R, mut output: W) -> Result<(), McpError>
where
    R: BufRead,
    W: Write,
{
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: McpRequest = serde_json::from_str(&line)?;
        if let Some(response) = server.handle_request(request) {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

struct ToolCallOutput {
    value: Value,
    is_error: bool,
}

impl ToolCallOutput {
    fn success(value: Value) -> Self {
        Self {
            value,
            is_error: false,
        }
    }

    fn error(value: Value) -> Self {
        Self {
            value,
            is_error: true,
        }
    }
}

fn service_response_result(response: JsonRpcResponse) -> Result<ToolCallOutput, McpError> {
    if let Some(error) = response.error {
        return Ok(ToolCallOutput::error(json!({
            "error": {
                "code": error.code,
                "message": error.message,
            }
        })));
    }
    Ok(ToolCallOutput::success(
        response.result.unwrap_or_else(|| json!(null)),
    ))
}

fn tool_descriptors() -> Vec<Value> {
    vec![
        tool(
            "service.health",
            "Return DbgAtlas service health.",
            json!({}),
        ),
        tool(
            "service.info",
            "Return DbgAtlas service information.",
            json!({}),
        ),
        tool(
            "debug.session.create",
            "Create a debug session from a dump or attach target.",
            json!({
                "type": "object",
                "properties": {
                    "project_root": { "type": "string" },
                    "target": { "type": "object" }
                },
                "required": ["project_root", "target"]
            }),
        ),
        tool(
            "debug.eval",
            "Execute a raw WinDbg command in an existing session.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "object" },
                    "command": { "type": "string" }
                },
                "required": ["session_id", "command"]
            }),
        ),
        tool(
            "debug.modules",
            "List modules for a debug session.",
            session_schema(),
        ),
        tool(
            "debug.threads",
            "List threads for a debug session.",
            session_schema(),
        ),
        tool(
            "debug.stack",
            "Get stack for a debug session.",
            session_schema(),
        ),
        tool(
            "debug.add_symbols",
            "Add a symbol path to a debug session.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "object" },
                    "symbol_path": { "type": "string" },
                    "reload": { "type": "boolean" }
                },
                "required": ["session_id", "symbol_path"]
            }),
        ),
        tool(
            "debug.read_memory",
            "Read virtual memory to an artifact.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "object" },
                    "address": {},
                    "length": { "type": "integer" }
                },
                "required": ["session_id", "address", "length"]
            }),
        ),
        tool(
            "debug.session.close",
            "Close a debug session.",
            session_schema(),
        ),
        tool(
            "debug.session.kill",
            "Kill a debug session worker.",
            session_schema(),
        ),
        tool(
            "operation.get",
            "Return an operation status and artifact refs.",
            operation_schema(),
        ),
        tool(
            "operation.cancel",
            "Cancel a running operation.",
            operation_schema(),
        ),
        tool(
            "operation.stream",
            "Return operation events.",
            operation_schema(),
        ),
        tool(
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
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn session_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session_id": { "type": "object" }
        },
        "required": ["session_id"]
    })
}

fn operation_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation_id": { "type": "object" }
        },
        "required": ["operation_id"]
    })
}

fn mcp_error_for(error: McpError) -> McpErrorObject {
    McpErrorObject {
        code: -32000,
        message: error.to_string(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpErrorObject>,
}

impl McpResponse {
    fn result(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, error: McpErrorObject) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpErrorObject {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceFactsParams {
    path: std::path::PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgatlas_service::{INTERNAL_WORKSPACE_DIR, ServiceHost};
    use std::io::Cursor;

    #[test]
    fn mcp_debug_workflow_uses_service_results_with_refs() {
        let temp = tempfile::tempdir().unwrap();
        let server = McpServer::with_host(ServiceHost::with_mock_workers());

        let create = server
            .call_tool(
                "debug.session.create",
                json!({
                    "project_root": temp.path(),
                    "target": { "kind": "dump", "path": "sample.dmp" }
                }),
            )
            .unwrap();
        let session_id = create["session_id"].clone();
        let eval = server
            .call_tool(
                "debug.eval",
                json!({
                    "session_id": session_id,
                    "command": ".echo from-mcp"
                }),
            )
            .unwrap();

        assert_eq!(eval["operation_status"], "success");
        assert!(eval["raw_output_ref"].get("id").is_some());
        assert_eq!(eval["artifact_refs"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn mcp_workspace_facts_reads_recording_layer() {
        let temp = tempfile::tempdir().unwrap();
        let server = McpServer::with_host(ServiceHost::with_mock_workers());
        let create = server
            .call_tool(
                "debug.session.create",
                json!({
                    "project_root": temp.path(),
                    "target": { "kind": "dump", "path": "sample.dmp" }
                }),
            )
            .unwrap();
        let session_id = create["session_id"].clone();
        server
            .call_tool(
                "debug.eval",
                json!({
                    "session_id": session_id,
                    "command": ".echo facts"
                }),
            )
            .unwrap();

        let facts = server
            .call_tool(
                "workspace.facts",
                json!({ "path": temp.path().join(INTERNAL_WORKSPACE_DIR) }),
            )
            .unwrap();

        assert_eq!(facts["command_audit"].as_array().unwrap().len(), 1);
        assert!(
            facts["operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation["capability"] == "debug.eval")
        );
    }

    #[test]
    fn stdio_notifications_do_not_write_responses() {
        let server = McpServer::with_host(ServiceHost::with_mock_workers());
        let input =
            Cursor::new(r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#);
        let mut output = Vec::new();

        serve_stdio(server, input, &mut output).unwrap();

        assert!(output.is_empty());
    }

    #[test]
    fn tool_execution_errors_are_marked_as_mcp_tool_errors() {
        let server = McpServer::with_host(ServiceHost::with_mock_workers());
        let response = server.handle_request(McpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": "debug.eval",
                "arguments": {
                    "session_id": { "id": "missing-session" },
                    "command": ".echo nope"
                }
            })),
        });

        let result = response.unwrap().result.unwrap();
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("session not found")
        );
    }
}
