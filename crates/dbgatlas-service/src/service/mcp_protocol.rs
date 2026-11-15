use super::*;

pub(super) const LATEST_VERSION: &str = "2025-06-18";
pub(super) const HEADER_DEFAULT_VERSION: &str = "2025-03-26";
pub(super) fn supported(version: &str) -> bool {
    matches!(version, "2024-11-05" | "2025-03-26" | "2025-06-18")
}

impl ServiceHost {
    /// The direct Rust entry point preserves legacy text-only results. HTTP
    /// selects its response shape from the per-request protocol version header.
    pub fn handle_mcp(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        self.handle_mcp_version(request, HEADER_DEFAULT_VERSION)
    }

    pub(super) fn handle_mcp_version(
        &self,
        request: JsonRpcRequest,
        version: &str,
    ) -> Option<JsonRpcResponse> {
        // Notifications must never execute a tool merely because its id is absent.
        // This stateless transport does not associate a cancellation request ID
        // with another client's call. Business cancellation is execution.cancel.
        if request.id.is_none() {
            return None;
        }
        let id = request.id.clone();
        if id
            .as_ref()
            .is_some_and(|id| !id.is_string() && !id.is_number())
            || request.jsonrpc != "2.0"
        {
            return Some(JsonRpcResponse::error(
                id,
                JsonRpcError {
                    code: -32600,
                    message: "invalid JSON-RPC request".into(),
                },
            ));
        }
        Some(match self.handle_mcp_method_version(request, version) {
            Ok(result) => JsonRpcResponse::result(id, result),
            Err(error) => JsonRpcResponse::error(id, mcp_error_for(error)),
        })
    }

    fn handle_mcp_method_version(
        &self,
        request: JsonRpcRequest,
        version: &str,
    ) -> Result<Value, ServiceError> {
        let params = request.params.unwrap_or_else(|| json!({}));
        match request.method.as_str() {
            "initialize" => {
                let requested = params["protocolVersion"].as_str().ok_or_else(|| {
                    ServiceError::Rpc("initialize requires protocolVersion".into())
                })?;
                let selected = if supported(requested) {
                    requested
                } else {
                    LATEST_VERSION
                };
                Ok(
                    json!({"protocolVersion":selected,"serverInfo":{"name":"dbgatlas-mcp","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}},"instructions":"Use session.exec for native commands. Wait expiry or disconnect does not cancel execution. Observe execution.get/session.get and read output with execution.read/session.read. Use session.help for guidance. Tasks and server notifications are not supported."}),
                )
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                if params.get("cursor").is_some_and(|v| !v.is_null()) {
                    return Err(ServiceError::Rpc(
                        "tools/list has a single page; cursor is invalid".into(),
                    ));
                }
                Ok(json!({"tools":mcp_tool_descriptors()}))
            }
            "tools/call" => {
                let call: ToolCallParams = serde_json::from_value(params)?;
                let arguments = call.arguments.unwrap_or_else(|| json!({}));
                let mut result = self.call_mcp_tool_output(&call.name, arguments.clone())?;
                // Observing a failed execution succeeds; accepting an action that
                // has already failed must not look like a successful tool action.
                if matches!(
                    call.name.as_str(),
                    "session.open"
                        | "session.exec"
                        | "session.interrupt"
                        | "session.close"
                        | "session.kill"
                        | "execution.cancel"
                ) {
                    let action = result
                        .value
                        .get("control_execution")
                        .or_else(|| result.value.get("execution"));
                    result.is_error |= action.is_some_and(|e| {
                        matches!(e["status"].as_str(), Some("failed" | "unknown"))
                    });
                }
                if result.is_error {
                    append_mcp_tool_error_log(&call.name, &arguments, &result);
                }
                if version == LATEST_VERSION {
                    let summary = tool_summary(&call.name, &result);
                    Ok(
                        json!({"structuredContent":result.value,"content":[{"type":"text","text":summary}],"isError":result.is_error}),
                    )
                } else {
                    Ok(
                        json!({"content":[{"type":"text","text":serde_json::to_string(&result.value)?}],"isError":result.is_error}),
                    )
                }
            }
            other => Err(ServiceError::Rpc(format!("unknown MCP method `{other}`"))),
        }
    }
}

/// Keep the structured body once. The text is a small navigation summary, not
/// a second serialized copy of a command preview, artifact list or output page.
fn tool_summary(name: &str, result: &ToolCallOutput) -> String {
    let value = &result.value;
    let mut summary = json!({"tool":name,"isError":result.is_error});
    for field in [
        "execution_id",
        "session_id",
        "status",
        "wait_expired",
        "has_more",
        "complete",
        "gap",
        "accepted",
        "code",
    ] {
        if let Some(value) = value.get(field) {
            summary[field] = value.clone();
        }
    }
    if let Some(execution) = value.get("execution") {
        summary["execution_id"] = execution["execution_id"].clone();
        summary["execution_status"] = execution["status"].clone();
    }
    summary["details"] = json!("See structuredContent; use session.help for workflow guidance.");
    serde_json::to_string(&summary).unwrap()
}
