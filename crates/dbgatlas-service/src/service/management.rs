//! Read-only management surface. Records remain owned by Workspace; the browser
//! never receives a generic RPC bridge or an arbitrary file-read endpoint.
use super::*;
use std::io::{Seek, SeekFrom};

fn response(status: u16, content_type: &str, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Bad Request",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn validate_browser(request: &rpc_http::HttpRequest, port: u16) -> bool {
    let Some(host) = request.headers.get("host") else {
        return false;
    };
    if ![
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
    ]
    .contains(&host.to_ascii_lowercase())
    {
        return false;
    }
    if request
        .headers
        .get("sec-fetch-site")
        .is_some_and(|v| v != "same-origin" && v != "none")
    {
        return false;
    }
    request
        .headers
        .get("origin")
        .is_none_or(|origin| origin == &format!("http://{host}"))
}

pub(super) fn handle_stream(
    stream: &mut TcpStream,
    host: &ServiceHost,
) -> Result<String, ServiceError> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = rpc_http::read_http_request_bounded(stream, 16 * 1024)?;
    if !validate_browser(&request, stream.local_addr()?.port()) {
        return Ok(response(
            403,
            "application/json",
            r#"{"error":"management requires a local host and same-origin requests"}"#,
        ));
    }
    if request.method == "GET" {
        return Ok(match request.path.split('?').next().unwrap_or("") {
            "/" => response(
                200,
                "text/html; charset=utf-8",
                include_str!("management/index.html"),
            ),
            "/app.js" => response(
                200,
                "text/javascript; charset=utf-8",
                include_str!("management/app.js"),
            ),
            "/style.css" => response(
                200,
                "text/css; charset=utf-8",
                include_str!("management/style.css"),
            ),
            _ => response(404, "text/plain", "Not found"),
        });
    }
    if request.method != "POST" {
        return Ok(response(405, "text/plain", "Method not allowed"));
    }
    if request.path != "/api/query" {
        return Ok(response(404, "text/plain", "Not found"));
    }
    if !request.headers.get("content-type").is_some_and(|v| {
        v.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
    }) {
        return Ok(response(
            400,
            "application/json",
            r#"{"error":"application/json is required"}"#,
        ));
    }
    let result = serde_json::from_slice::<Value>(&request.body)
        .map_err(ServiceError::from)
        .and_then(|params| host.management_query(&params));
    Ok(match result {
        Ok(value) => response(
            200,
            "application/json; charset=utf-8",
            &serde_json::to_string(&value)?,
        ),
        Err(error) => response(
            400,
            "application/json; charset=utf-8",
            &json!({"error":error.to_string()}).to_string(),
        ),
    })
}

impl ServiceHost {
    pub(super) fn management_query(&self, params: &Value) -> Result<Value, ServiceError> {
        let action = params["action"].as_str().unwrap_or("overview");
        if action == "overview" {
            let workspaces = self.history.workspaces()?;
            let roots: Vec<_> = workspaces
                .iter()
                .map(|root| {
                    if root.ends_with(INTERNAL_WORKSPACE_DIR) {
                        root.parent().unwrap_or(root)
                    } else {
                        root
                    }
                })
                .collect();
            let state = self.lock_state()?;
            return Ok(
                json!({"version":env!("CARGO_PKG_VERSION"),"service_instance":self.service_instance,"projects":roots,"workspace_roots":workspaces,"active_sessions":state.summaries.values().filter(|s| !s.lifecycle.is_terminal()).count()}),
            );
        }
        if !matches!(
            action,
            "workspace" | "output" | "history" | "provenance" | "facts"
        ) {
            return Err(ServiceError::Rpc("unsupported management query".into()));
        }
        let root = match params["workspace_root"].as_str() {
            Some(path) => PathBuf::from(path),
            None => Path::new(
                params["project_root"]
                    .as_str()
                    .ok_or_else(|| ServiceError::Rpc("project_root is required".into()))?,
            )
            .join(INTERNAL_WORKSPACE_DIR),
        };
        if !root.is_absolute() {
            return Err(ServiceError::Rpc("workspace path must be absolute".into()));
        }
        let workspace = self.history.lookup(root)?;
        if action == "facts" {
            return Ok(json!(workspace.facts()?));
        }
        if action == "output" {
            return read_output(&workspace, params);
        }
        if action == "history" {
            let after = params["after"].as_i64().unwrap_or(0);
            return Ok(
                json!({"entries":workspace.history_entries(params["execution_id"].as_str(),params["session_id"].as_str(),after,100)?}),
            );
        }
        if action == "provenance" {
            let id: ArtifactRef = serde_json::from_value(params["artifact_id"].clone())?;
            let artifact = workspace
                .get_artifact(&id)?
                .ok_or_else(|| ServiceError::Rpc("artifact not found".into()))?;
            let consumers: Vec<_> = workspace
                .artifact_consumers(&id)?
                .into_iter()
                .map(|s| s.session_id)
                .collect();
            return Ok(json!({"artifact":artifact,"consumer_sessions":consumers}));
        }
        let filter: dbgatlas_workspace::HistoryFilter = serde_json::from_value(params.clone())?;
        if matches!((filter.start_ms,filter.end_ms),(Some(s),Some(e)) if s>=e) {
            return Err(ServiceError::Rpc(
                "end_ms must be later than start_ms".into(),
            ));
        }
        let persisted = workspace.sessions()?;
        let (sessions, overrides) = {
            let state = self.lock_state()?;
            let mut sessions = Vec::with_capacity(persisted.len());
            for saved in &persisted {
                let live = state
                    .summaries
                    .get(saved.session_id.id.as_str())
                    .filter(|s| s.workspace.root == workspace.root());
                sessions.push(session_view(live.unwrap_or(saved), live.is_some())?);
            }
            let mut overrides = serde_json::Map::new();
            for op in state.operations.values() {
                // Completed and archived operations already have the authoritative database row.
                if execution::terminal(&op.status) && op.details.output_error.is_none() {
                    continue;
                }
                let session = op
                    .session_id
                    .as_ref()
                    .or(op.details.session_id.as_ref())
                    .and_then(|sid| state.summaries.get(sid.id.as_str()));
                if session.is_some_and(|s| s.workspace.root == workspace.root()) {
                    overrides.insert(
                        op.operation_id.id.to_string(),
                        json!({"status":op.status,"summary":op.summary,"artifacts":op.artifacts,
                        "raw_output":op.raw_output,"output_error":op.details.output_error}),
                    );
                }
            }
            (sessions, Value::Object(overrides))
        };
        let mut response = serde_json::to_value(workspace.operation_page(&filter, &overrides)?)?;
        response["workspace"] = json!(workspace.info());
        response["sessions"] = json!(sessions);
        response["warnings"] = if workspace.root().is_dir() {
            json!([])
        } else {
            json!(["项目目录不可用；仍可查看中心库中的历史记录。"])
        };
        Ok(response)
    }
}

fn session_view(session: &dbgatlas_model::Session, current: bool) -> Result<Value, ServiceError> {
    let mut value = serde_json::to_value(session)?;
    value["current_service"] = json!(current);
    if !current && !session.lifecycle.is_terminal() {
        value["last_known_lifecycle"] = value["lifecycle"].clone();
        value["lifecycle"] = json!("unknown");
        value["connection"] = json!("unknown");
        value["target_state"]["state"] = json!("unknown");
    }
    Ok(value)
}

fn read_output(workspace: &Workspace, params: &Value) -> Result<Value, ServiceError> {
    let id: ArtifactRef = serde_json::from_value(params["artifact_id"].clone())?;
    let artifact = workspace
        .get_artifact(&id)?
        .ok_or_else(|| ServiceError::Rpc("artifact not found".into()))?;
    let mut file = workspace.open_artifact_read(&artifact.relative_path)?;
    let extension = artifact
        .relative_path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "txt" | "log" | "md" | "json" | "jsonl" | "csv"
    ) {
        return Ok(json!({"artifact":artifact,"binary":true,"text":"","has_more":false}));
    }
    let length = file.metadata()?.len();
    let offset = params["offset"].as_u64().unwrap_or(0);
    if offset > length {
        return Err(ServiceError::Rpc(
            "output changed or offset is beyond its end; reload output".into(),
        ));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; (length - offset).min(65536) as usize];
    let count = file.read(&mut bytes)?;
    bytes.truncate(count);
    let mut lossy = false;
    let mut checked = 0;
    while checked < bytes.len() {
        match std::str::from_utf8(&bytes[checked..]) {
            Ok(_) => break,
            Err(e) => {
                checked += e.valid_up_to();
                if let Some(n) = e.error_len() {
                    lossy = true;
                    checked += n;
                } else if offset + (bytes.len() as u64) < length {
                    bytes.truncate(checked);
                    break;
                } else {
                    lossy = true;
                    break;
                }
            }
        }
    }
    let next = offset + bytes.len() as u64;
    Ok(
        json!({"artifact":artifact,"binary":false,"text":String::from_utf8_lossy(&bytes),"next_offset":next,"byte_len":length,"has_more":next < length,"lossy":lossy}),
    )
}
