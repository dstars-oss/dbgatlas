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
            let state = self.lock_state()?;
            let mut roots: Vec<_> = state
                .summaries
                .values()
                .filter_map(|s| s.workspace.root.parent().map(Path::to_path_buf))
                .collect();
            roots.sort();
            roots.dedup();
            return Ok(
                json!({"version":env!("CARGO_PKG_VERSION"),"service_instance":self.service_instance,"projects":roots,"active_sessions":state.summaries.values().filter(|s| !s.lifecycle.is_terminal()).count()}),
            );
        }
        if !matches!(action, "workspace" | "output") {
            return Err(ServiceError::Rpc("unsupported management query".into()));
        }
        let project = Path::new(
            params["project_root"]
                .as_str()
                .ok_or_else(|| ServiceError::Rpc("project_root is required".into()))?,
        );
        if !project.is_absolute() {
            return Err(ServiceError::Rpc(
                "project_root must be an absolute path".into(),
            ));
        }
        // Opening a management view never creates a workspace or analysis data.
        let workspace = Workspace::open(project.join(INTERNAL_WORKSPACE_DIR))?;
        if action == "output" {
            return read_output(&workspace, params);
        }
        let root = fs::canonicalize(workspace.root())?;
        let live: HashMap<_, _> = self
            .lock_state()?
            .summaries
            .values()
            .filter(|s| fs::canonicalize(&s.workspace.root).ok().as_ref() == Some(&root))
            .map(|s| (s.session_id.id.to_string(), s.clone()))
            .collect();
        let mut sessions = HashMap::new();
        let mut warnings = Vec::new();
        for directory in ["sessions", "recordings"] {
            let path = workspace.root().join("artifacts").join(directory);
            if !path.exists() {
                continue;
            }
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let relative = PathBuf::from("artifacts")
                    .join(directory)
                    .join(entry.file_name())
                    .join("session.json");
                if !workspace.root().join(&relative).exists() {
                    continue;
                }
                let loaded = (|| -> Result<Value, ServiceError> {
                    let file = workspace.open_artifact_read(&relative)?;
                    let session: dbgatlas_model::Session =
                        serde_json::from_reader(file.take(1024 * 1024))?;
                    session_view(&session, false)
                })();
                match loaded {
                    Ok(value) => {
                        sessions.insert(
                            value["session_id"]["id"].as_str().unwrap_or("").to_owned(),
                            value,
                        );
                    }
                    Err(error) => warnings.push(format!("{}: {error}", relative.display())),
                }
            }
        }
        for (id, session) in &live {
            sessions.insert(id.clone(), session_view(session, true)?);
        }
        let mut sessions: Vec<_> = sessions.into_values().collect();
        sessions.sort_by_key(|s| {
            std::cmp::Reverse(s["created_at"]["unix_millis"].as_u64().unwrap_or(0))
        });
        let mut operations = workspace.list_operations()?;
        let current_operations = self.lock_state()?.operations.clone();
        for op in &mut operations {
            let current = current_operations
                .get(op.operation_id.id.as_str())
                .filter(|current| {
                    let sid = current
                        .session_id
                        .as_ref()
                        .or(current.details.session_id.as_ref());
                    sid == op.details.session_id.as_ref()
                        && sid.is_some_and(|id| live.contains_key(id.id.as_str()))
                });
            if let Some(current) = current {
                // Execution can finish even when its final journal append fails.
                // Keep that known result and its separate archival error visible.
                op.status = current.status.clone();
                op.summary = current.summary.clone();
                op.details = current.details.clone();
                op.artifacts = current.artifacts.clone();
                op.raw_output = current.raw_output.clone();
            } else if matches!(
                op.status,
                OperationStatus::Running | OperationStatus::Pending
            ) {
                op.status = OperationStatus::Unknown;
            }
        }
        let total_operations = operations.len();
        operations.sort_by(|a, b| {
            b.created_at
                .unix_millis
                .cmp(&a.created_at.unix_millis)
                .then_with(|| b.operation_id.to_string().cmp(&a.operation_id.to_string()))
        });
        let session_filter = params["session_id"].as_str().unwrap_or("");
        let status_filter = params["status"].as_str().unwrap_or("");
        let needle = params["search"].as_str().unwrap_or("").to_lowercase();
        let operations: Vec<_> = operations
            .into_iter()
            .filter(|op| {
                (session_filter.is_empty()
                    || op
                        .details
                        .session_id
                        .as_ref()
                        .is_some_and(|s| s.id.as_str() == session_filter))
                    && (status_filter.is_empty()
                        || serde_json::to_value(&op.status)
                            .ok()
                            .and_then(|s| s.as_str().map(str::to_owned))
                            .as_deref()
                            == Some(status_filter))
                    && (needle.is_empty()
                        || format!(
                            "{} {} {} {}",
                            op.operation_id,
                            op.capability,
                            op.summary,
                            op.details.input.as_ref().unwrap_or(&Value::Null)
                        )
                        .to_lowercase()
                        .contains(&needle))
            })
            .collect();
        let count = operations.len();
        let offset = params["offset"].as_u64().unwrap_or(0).min(count as u64) as usize;
        let limit = params["limit"].as_u64().unwrap_or(40).clamp(1, 100) as usize;
        let page = &operations[offset..(offset + limit).min(count)];
        let artifacts = workspace.list_artifacts()?;
        let selected_artifacts: Vec<_> = artifacts
            .iter()
            .filter(|a| {
                page.iter().any(|op| {
                    op.artifacts.contains(&a.artifact_id)
                        || op.raw_output.as_ref() == Some(&a.artifact_id)
                })
            })
            .collect();
        Ok(
            json!({"workspace":workspace.info(),"sessions":sessions,"operations":page,"artifacts":selected_artifacts,"total_operations":total_operations,"total_artifacts":artifacts.len(),"filtered_count":count,"offset":offset,"has_more":offset + page.len() < count,"warnings":warnings}),
        )
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
    let mut file = workspace.open_artifact_read(&artifact.relative_path)?;
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
