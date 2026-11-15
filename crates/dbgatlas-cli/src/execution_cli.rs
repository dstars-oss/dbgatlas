use super::*;
use clap::Args;

type Request = (Option<SocketAddr>, &'static str, Value);

#[derive(Args)]
pub(super) struct Action {
    session_id: String,
    /// Observation budget only; expiry never cancels the execution.
    #[arg(long,default_value_t=1000,value_parser=clap::value_parser!(u64).range(0..=30000))]
    wait_ms: u64,
    #[arg(long)]
    endpoint: Option<SocketAddr>,
}
#[derive(Args)]
pub(super) struct Observe {
    #[arg(long)]
    after_seq: Option<u64>,
    #[arg(long,default_value_t=0,value_parser=clap::value_parser!(u64).range(0..=30000))]
    wait_ms: u64,
    #[arg(long)]
    endpoint: Option<SocketAddr>,
}
#[derive(Args)]
pub(super) struct ReadOutput {
    #[arg(long)]
    cursor: Option<String>,
    #[arg(long,default_value_t=8192,value_parser=clap::value_parser!(u64).range(4..=65536))]
    max_bytes: u64,
    #[arg(long)]
    endpoint: Option<SocketAddr>,
}
#[derive(Args)]
#[group(id = "open", multiple = true)]
pub(super) struct Open {
    #[arg(long)]
    project_root: PathBuf,
    #[arg(long, group = "target")]
    file: Option<PathBuf>,
    #[arg(long, group = "target")]
    artifact_ref: Option<String>,
    #[arg(long,group="target",value_parser=clap::value_parser!(u32).range(1..))]
    attach: Option<u32>,
    #[arg(long, group = "target")]
    launch: Option<PathBuf>,
    /// Taking debug control requires explicit invasive mode.
    #[arg(long,requires="attach",conflicts_with_all=["file","artifact_ref","launch"],value_parser=["noninvasive","invasive"])]
    attach_mode: Option<String>,
    #[arg(long,value_parser=["default","active_interactive_user"])]
    worker_identity: Option<String>,
    #[arg(long,value_parser=clap::value_parser!(u64).range(1..))]
    startup_timeout_ms: Option<u64>,
    #[arg(long,default_value_t=1000,value_parser=clap::value_parser!(u64).range(0..=30000))]
    wait_ms: u64,
    #[arg(long)]
    endpoint: Option<SocketAddr>,
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
pub(super) enum SessionCommand {
    /// Open a dump/TTD file, Artifact, external process or new process.
    #[command(group(clap::ArgGroup::new("target").required(true).multiple(false)))]
    Open(Open),
    /// Execute one unchanged native command; never retry automatically.
    Exec {
        #[command(flatten)]
        action: Action,
        #[arg(allow_hyphen_values = true)]
        command: String,
        #[arg(long, requires = "expected_service_instance")]
        client_request_id: Option<String>,
        #[arg(long, requires = "client_request_id")]
        expected_service_instance: Option<String>,
    },
    Get {
        session_id: String,
        #[command(flatten)]
        observe: Observe,
    },
    List {
        #[arg(long,default_value="debug",value_parser=["debug","recording","all"])]
        kind: String,
        #[arg(long)]
        include_terminal: bool,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Interrupt {
        #[command(flatten)]
        action: Action,
        #[arg(long)]
        expected_run_epoch: Option<u64>,
    },
    Close(Action),
    Kill(Action),
    Read {
        session_id: String,
        #[command(flatten)]
        output: ReadOutput,
    },
    /// Request focused backend guidance (use --help for CLI syntax).
    Help {
        #[arg(default_value="overview",value_parser=["overview","execution","output","control"])]
        topic: String,
        #[arg(long, conflicts_with = "backend")]
        session_id: Option<String>,
        #[arg(long,value_parser=["dbgeng"])]
        backend: Option<String>,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
}
#[derive(Subcommand)]
pub(super) enum ExecutionCommand {
    Get {
        execution_id: String,
        #[command(flatten)]
        observe: Observe,
    },
    Read {
        execution_id: String,
        #[command(flatten)]
        output: ReadOutput,
    },
    Cancel {
        execution_id: String,
        #[arg(long,default_value_t=1000,value_parser=clap::value_parser!(u64).range(0..=30000))]
        wait_ms: u64,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    List {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long,value_parser=["pending","running","success","failed","canceled","unknown"])]
        status: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long,default_value_t=20,value_parser=clap::value_parser!(u64).range(1..=100))]
        limit: u64,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
}
fn omit_nulls(mut value: Value) -> Value {
    if let Some(fields) = value.as_object_mut() {
        fields.retain(|_, value| !value.is_null());
    }
    value
}
impl SessionCommand {
    pub(super) fn request(self) -> Result<Request> {
        let (endpoint, method, params) = match self {
            Self::Open(open) => {
                if open.attach_mode.is_some() && open.attach.is_none() {
                    anyhow::bail!("--attach-mode requires --attach");
                }
                if open.launch.is_none() && !open.args.is_empty() {
                    anyhow::bail!("arguments after -- require --launch");
                }
                let target = if let Some(file) = open.file {
                    json!({"kind":"file","path":absolute_path(file)?})
                } else if let Some(reference) = open.artifact_ref {
                    json!({"kind":"file","artifact_ref":reference})
                } else if let Some(pid) = open.attach {
                    json!({"kind":"attach","pid":pid,"mode":open.attach_mode.unwrap_or_else(||"noninvasive".into())})
                } else {
                    json!({"kind":"launch","executable":absolute_path(open.launch.context("a target is required")?)?,"args":open.args})
                };
                (
                    open.endpoint,
                    "session.open",
                    json!({"project_root":absolute_path(open.project_root)?,"backend":"dbgeng","target":target,"worker_identity":open.worker_identity,"startup_timeout_ms":open.startup_timeout_ms,"wait_ms":open.wait_ms}),
                )
            }
            Self::Exec {
                action,
                command,
                client_request_id,
                expected_service_instance,
            } => (
                action.endpoint,
                "session.exec",
                json!({"session_id":action.session_id,"command":command,"wait_ms":action.wait_ms,"client_request_id":client_request_id,"expected_service_instance":expected_service_instance}),
            ),
            Self::Get {
                session_id,
                observe,
            } => (
                observe.endpoint,
                "session.get",
                json!({"session_id":session_id,"after_seq":observe.after_seq,"wait_ms":observe.wait_ms}),
            ),
            Self::List {
                kind,
                include_terminal,
                endpoint,
            } => (
                endpoint,
                "session.list",
                json!({"kind":kind,"include_terminal":include_terminal}),
            ),
            Self::Interrupt {
                action,
                expected_run_epoch,
            } => (
                action.endpoint,
                "session.interrupt",
                json!({"session_id":action.session_id,"expected_run_epoch":expected_run_epoch,"wait_ms":action.wait_ms}),
            ),
            Self::Close(action) => (
                action.endpoint,
                "session.close",
                json!({"session_id":action.session_id,"wait_ms":action.wait_ms}),
            ),
            Self::Kill(action) => (
                action.endpoint,
                "session.kill",
                json!({"session_id":action.session_id,"wait_ms":action.wait_ms}),
            ),
            Self::Read { session_id, output } => (
                output.endpoint,
                "session.read",
                json!({"session_id":session_id,"cursor":output.cursor,"max_bytes":output.max_bytes}),
            ),
            Self::Help {
                topic,
                session_id,
                backend,
                endpoint,
            } => (
                endpoint,
                "session.help",
                json!({"topic":topic,"session_id":session_id,"backend":backend}),
            ),
        };
        Ok((endpoint, method, omit_nulls(params)))
    }
}
impl ExecutionCommand {
    pub(super) fn request(self) -> Result<Request> {
        let (endpoint, method, params) = match self {
            Self::Get {
                execution_id,
                observe,
            } => (
                observe.endpoint,
                "execution.get",
                json!({"execution_id":execution_id,"after_seq":observe.after_seq,"wait_ms":observe.wait_ms}),
            ),
            Self::Read {
                execution_id,
                output,
            } => (
                output.endpoint,
                "execution.read",
                json!({"execution_id":execution_id,"cursor":output.cursor,"max_bytes":output.max_bytes}),
            ),
            Self::Cancel {
                execution_id,
                wait_ms,
                endpoint,
            } => (
                endpoint,
                "execution.cancel",
                json!({"execution_id":execution_id,"wait_ms":wait_ms}),
            ),
            Self::List {
                project_root,
                session_id,
                status,
                cursor,
                limit,
                endpoint,
            } => (
                endpoint,
                "execution.list",
                json!({"project_root":absolute_path(project_root)?,"session_id":session_id,"status":status,"cursor":cursor,"limit":limit}),
            ),
        };
        Ok((endpoint, method, omit_nulls(params)))
    }
}
pub(super) fn run((endpoint, method, params): Request, as_json: bool) -> Result<()> {
    print_rpc_response(call_service(endpoint, method, params)?, as_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(args: &[&str]) -> Request {
        let cli =
            Cli::try_parse_from(std::iter::once("dbgatlas").chain(args.iter().copied())).unwrap();
        match cli.command {
            Commands::Session { command } => command.request().unwrap(),
            Commands::Execution { command } => command.request().unwrap(),
            _ => panic!("unexpected command"),
        }
    }
    #[test]
    fn raw_commands_and_cursors_are_not_reinterpreted() {
        let command = r#".echo "a;b"; r @$t0 = 1
.echo tail"#;
        let (_, method, params) = request(&[
            "session",
            "exec",
            "s1",
            command,
            "--wait-ms",
            "0",
            "--client-request-id",
            "key",
            "--expected-service-instance",
            "service1",
        ]);
        assert_eq!(method, "session.exec");
        assert_eq!(params["command"], command);
        assert_eq!(params["wait_ms"], 0);
        let (_, method, params) = request(&[
            "execution",
            "read",
            "e1",
            "--cursor",
            "opaque.token",
            "--max-bytes",
            "4",
        ]);
        assert_eq!(method, "execution.read");
        assert_eq!(params["cursor"], "opaque.token");
        assert!(
            Cli::try_parse_from([
                "dbgatlas",
                "session",
                "exec",
                "s1",
                "g",
                "--timeout-ms",
                "3"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "dbgatlas",
                "session",
                "exec",
                "s1",
                "g",
                "--client-request-id",
                "key"
            ])
            .is_err()
        );
    }
    #[test]
    fn open_targets_are_exclusive_and_control_is_explicit() {
        let (_, _, params) = request(&["session", "open", "--project-root", ".", "--attach", "42"]);
        assert_eq!(params["target"]["mode"], "noninvasive");
        let (_, _, params) = request(&[
            "session",
            "open",
            "--project-root",
            ".",
            "--attach",
            "42",
            "--attach-mode",
            "invasive",
        ]);
        assert_eq!(params["target"]["mode"], "invasive");
        let (_, _, params) = request(&[
            "session",
            "open",
            "--project-root",
            ".",
            "--launch",
            "app.exe",
            "--",
            "--flag",
            "a b",
        ]);
        assert_eq!(params["target"]["args"], json!(["--flag", "a b"]));
        assert!(
            Cli::try_parse_from(["dbgatlas", "session", "open", "--project-root", "."]).is_err()
        );
        assert!(
            Cli::try_parse_from([
                "dbgatlas",
                "session",
                "open",
                "--project-root",
                ".",
                "--file",
                "a.dmp",
                "--attach",
                "42"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "dbgatlas",
                "session",
                "open",
                "--project-root",
                ".",
                "--file",
                "a.dmp",
                "--attach-mode",
                "invasive"
            ])
            .is_err()
        );
    }
    #[test]
    fn cli_failures_are_nonzero_but_running_and_control_success_are_not_errors() {
        let response = |result| JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            result: Some(result),
            error: None,
        };
        assert!(
            rpc_failure(&response(
                json!({"execution":{"execution_id":"e1","status":"failed"}})
            ))
            .is_some()
        );
        assert!(
            rpc_failure(&response(
                json!({"execution":{"status":"running"},"wait_expired":true})
            ))
            .is_none()
        );
        assert!(
            rpc_failure(&response(
                json!({"execution":{"status":"failed"},"control_execution":{"status":"success"}})
            ))
            .is_none()
        );
        assert!(rpc_failure(&response(json!({"status":"failed","execution_id":"e1"}))).is_none());
    }
}
