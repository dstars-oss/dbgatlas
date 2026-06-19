use anyhow::Result;
use clap::{Parser, Subcommand};
use dbgatlas_debug::DebugTarget;
use dbgatlas_recording::RecordingTarget;
use dbgatlas_service::{
    DEFAULT_SERVICE_UPDATE_TIMEOUT_MS, JsonRpcRequest, ServiceConfig, ServiceHost,
    WindowsServiceApplyUpdateOptions, WindowsServiceInstallOptions, WindowsServiceRunOptions,
    WindowsServiceUninstallOptions, apply_windows_service_update, install_windows_service,
    installed_client_config, invoke_http_json_rpc, run_http_service,
    run_windows_service_dispatcher, start_windows_service, status_windows_service,
    stop_windows_service, uninstall_windows_service,
};
use dbgatlas_workspace::{Workspace, WorkspaceInitOptions};
use serde_json::json;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dbgatlas")]
#[command(about = "DbgAtlas command line interface")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
    Recording {
        #[command(subcommand)]
        command: RecordingCommand,
    },
    Native {
        #[command(subcommand)]
        command: NativeCommand,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    Init {
        path: PathBuf,
        #[arg(long)]
        with_inputs: bool,
    },
    Info {
        path: PathBuf,
    },
    Facts {
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum ServiceCommand {
    Run {
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
        #[arg(long, hide = true)]
        windows_service: bool,
        #[arg(long, hide = true)]
        config: Option<PathBuf>,
        #[arg(long, hide = true)]
        token_file: Option<PathBuf>,
    },
    Health {
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Info {
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Install {
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: SocketAddr,
        #[arg(long)]
        force: bool,
    },
    Start,
    Stop,
    Status,
    Uninstall {
        #[arg(long)]
        purge: bool,
    },
    #[command(hide = true)]
    ApplyUpdate {
        #[arg(long)]
        source_dir: PathBuf,
        #[arg(long, default_value_t = DEFAULT_SERVICE_UPDATE_TIMEOUT_MS)]
        timeout_ms: u64,
        #[arg(long)]
        no_restart: bool,
    },
}

#[derive(Subcommand)]
enum DebugCommand {
    Session {
        #[command(subcommand)]
        command: DebugSessionCommand,
    },
    Eval {
        session_id: String,
        command: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Modules {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Threads {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Stack {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    ReadMemory {
        session_id: String,
        #[arg(long)]
        address: String,
        #[arg(long)]
        length: u64,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    AddSymbols {
        session_id: String,
        symbol_path: String,
        #[arg(long)]
        reload: bool,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand)]
enum DebugSessionCommand {
    Create {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        dump: Option<PathBuf>,
        #[arg(long)]
        attach: Option<u32>,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Close {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Kill {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand)]
enum RecordingCommand {
    Start {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        launch: Option<PathBuf>,
        #[arg(long)]
        attach: Option<u32>,
        #[arg(last = true)]
        args: Vec<String>,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Status {
        recording_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Stop {
        recording_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Cancel {
        recording_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
    Kill {
        recording_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand)]
enum NativeCommand {
    Version,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Workspace { command } => run_workspace(command, cli.json),
        Commands::Service { command } => run_service(command, cli.json),
        Commands::Debug { command } => run_debug(command, cli.json),
        Commands::Recording { command } => run_recording(command, cli.json),
        Commands::Native { command } => run_native(command, cli.json),
    }
}

fn run_workspace(command: WorkspaceCommand, as_json: bool) -> Result<()> {
    match command {
        WorkspaceCommand::Init { path, with_inputs } => {
            let workspace = Workspace::init(
                path,
                WorkspaceInitOptions {
                    create_inputs: with_inputs,
                },
            )?;
            if as_json {
                print_json(json!({
                    "root": workspace.root(),
                    "manifest": workspace.manifest_path(),
                    "workspace_id": workspace.manifest().workspace_id,
                }))?;
            } else {
                println!("initialized workspace: {}", workspace.root().display());
                println!("manifest: {}", workspace.manifest_path().display());
            }
        }
        WorkspaceCommand::Info { path } => {
            let workspace = Workspace::open(path)?;
            let info = workspace.info();
            if as_json {
                print_json(serde_json::to_value(&info)?)?;
            } else {
                println!("workspace: {}", info.root.display());
                println!("workspace id: {}", info.manifest.workspace_id);
                println!("schema version: {}", info.manifest.schema_version);
                println!("artifacts dir: {}", info.has_artifacts_dir);
                println!("analysis dir: {}", info.has_analysis_dir);
                println!("inputs dir: {}", info.has_inputs_dir);
            }
        }
        WorkspaceCommand::Facts { path } => {
            let workspace = Workspace::open(path)?;
            let facts = workspace.facts()?;
            if as_json {
                print_json(serde_json::to_value(&facts)?)?;
            } else {
                println!("workspace: {}", workspace.root().display());
                println!("artifacts: {}", facts.artifacts.len());
                println!("operations: {}", facts.operations.len());
                println!("command audit records: {}", facts.command_audit.len());
            }
        }
    }
    Ok(())
}

fn run_service(command: ServiceCommand, as_json: bool) -> Result<()> {
    match command {
        ServiceCommand::Run {
            bind,
            token,
            windows_service,
            config,
            token_file,
        } => {
            if windows_service {
                let config_path = config.ok_or_else(|| {
                    anyhow::anyhow!("--config is required with --windows-service")
                })?;
                let token_file = token_file.ok_or_else(|| {
                    anyhow::anyhow!("--token-file is required with --windows-service")
                })?;
                return Ok(run_windows_service_dispatcher(WindowsServiceRunOptions {
                    config_path,
                    token_file,
                })?);
            }
            let config = ServiceConfig {
                bind,
                bearer_token: token,
            };
            if !as_json {
                println!(
                    "DbgAtlas service RPC listening on http://{}/rpc",
                    config.bind
                );
                println!("DbgAtlas MCP listening on http://{}/mcp", config.bind);
            }
            run_http_service(config, ServiceHost::with_process_workers()?)?;
        }
        ServiceCommand::Health { endpoint, token } => {
            let response = call_service(endpoint, token, "service.health", json!({}))?;
            print_rpc_response(response, as_json)?;
        }
        ServiceCommand::Info { endpoint, token } => {
            let response = call_service(endpoint, token, "service.info", json!({}))?;
            print_rpc_response(response, as_json)?;
        }
        ServiceCommand::Install { bind, force } => {
            let result = install_windows_service(WindowsServiceInstallOptions { bind, force })?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::Start => {
            let result = start_windows_service()?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::Stop => {
            let result = stop_windows_service()?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::Status => {
            let result = status_windows_service()?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::Uninstall { purge } => {
            let result = uninstall_windows_service(WindowsServiceUninstallOptions { purge })?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::ApplyUpdate {
            source_dir,
            timeout_ms,
            no_restart,
        } => {
            let result = apply_windows_service_update(WindowsServiceApplyUpdateOptions {
                source_dir,
                restart: !no_restart,
                timeout_ms,
            })?;
            print_service_command_result(result, as_json)?;
        }
    }
    Ok(())
}

fn run_debug(command: DebugCommand, as_json: bool) -> Result<()> {
    match command {
        DebugCommand::Session { command } => run_debug_session(command, as_json),
        DebugCommand::Eval {
            session_id,
            command,
            endpoint,
            token,
        } => {
            let response = call_service(
                endpoint,
                token,
                "debug.eval",
                json!({
                    "session_id": { "id": session_id },
                    "command": command,
                }),
            )?;
            print_rpc_response(response, as_json)
        }
        DebugCommand::Modules {
            session_id,
            endpoint,
            token,
        } => call_session_tool(endpoint, token, "debug.modules", session_id, as_json),
        DebugCommand::Threads {
            session_id,
            endpoint,
            token,
        } => call_session_tool(endpoint, token, "debug.threads", session_id, as_json),
        DebugCommand::Stack {
            session_id,
            endpoint,
            token,
        } => call_session_tool(endpoint, token, "debug.stack", session_id, as_json),
        DebugCommand::ReadMemory {
            session_id,
            address,
            length,
            endpoint,
            token,
        } => {
            let response = call_service(
                endpoint,
                token,
                "debug.read_memory",
                json!({
                    "session_id": { "id": session_id },
                    "address": address,
                    "length": length,
                }),
            )?;
            print_rpc_response(response, as_json)
        }
        DebugCommand::AddSymbols {
            session_id,
            symbol_path,
            reload,
            endpoint,
            token,
        } => {
            let response = call_service(
                endpoint,
                token,
                "debug.add_symbols",
                json!({
                    "session_id": { "id": session_id },
                    "symbol_path": symbol_path,
                    "reload": reload,
                }),
            )?;
            print_rpc_response(response, as_json)
        }
    }
}

fn run_debug_session(command: DebugSessionCommand, as_json: bool) -> Result<()> {
    match command {
        DebugSessionCommand::Create {
            project_root,
            dump,
            attach,
            endpoint,
            token,
        } => {
            let project_root = absolute_path(project_root)?;
            let target = match (dump, attach) {
                (Some(path), None) => serde_json::to_value(DebugTarget::Dump {
                    path: std::fs::canonicalize(path)?,
                })?,
                (None, Some(pid)) => serde_json::to_value(DebugTarget::Attach { pid })?,
                _ => anyhow::bail!("provide exactly one of --dump or --attach"),
            };
            let response = call_service(
                endpoint,
                token,
                "debug.session.create",
                json!({
                    "project_root": project_root,
                    "target": target,
                }),
            )?;
            print_rpc_response(response, as_json)
        }
        DebugSessionCommand::Close {
            session_id,
            endpoint,
            token,
        } => call_session_tool(endpoint, token, "debug.session.close", session_id, as_json),
        DebugSessionCommand::Kill {
            session_id,
            endpoint,
            token,
        } => call_session_tool(endpoint, token, "debug.session.kill", session_id, as_json),
    }
}

fn run_recording(command: RecordingCommand, as_json: bool) -> Result<()> {
    match command {
        RecordingCommand::Start {
            project_root,
            launch,
            attach,
            args,
            endpoint,
            token,
        } => {
            let project_root = absolute_path(project_root)?;
            let target = match (launch, attach) {
                (Some(executable), None) => serde_json::to_value(RecordingTarget::Launch {
                    executable: absolute_path(executable)?,
                    args,
                })?,
                (None, Some(pid)) => {
                    if !args.is_empty() {
                        anyhow::bail!("--attach does not accept launch args");
                    }
                    serde_json::to_value(RecordingTarget::Attach { pid })?
                }
                _ => anyhow::bail!("provide exactly one of --launch or --attach"),
            };
            let response = call_service(
                endpoint,
                token,
                "recording.start",
                json!({
                    "project_root": project_root,
                    "target": target,
                }),
            )?;
            print_rpc_response(response, as_json)
        }
        RecordingCommand::Status {
            recording_id,
            endpoint,
            token,
        } => call_recording_tool(endpoint, token, "recording.status", recording_id, as_json),
        RecordingCommand::Stop {
            recording_id,
            endpoint,
            token,
        } => call_recording_tool(endpoint, token, "recording.stop", recording_id, as_json),
        RecordingCommand::Cancel {
            recording_id,
            endpoint,
            token,
        } => call_recording_tool(endpoint, token, "recording.cancel", recording_id, as_json),
        RecordingCommand::Kill {
            recording_id,
            endpoint,
            token,
        } => call_recording_tool(endpoint, token, "recording.kill", recording_id, as_json),
    }
}

fn call_session_tool(
    endpoint: Option<SocketAddr>,
    token: Option<String>,
    method: &str,
    session_id: String,
    as_json: bool,
) -> Result<()> {
    let response = call_service(
        endpoint,
        token,
        method,
        json!({
            "session_id": { "id": session_id },
        }),
    )?;
    print_rpc_response(response, as_json)
}

fn call_recording_tool(
    endpoint: Option<SocketAddr>,
    token: Option<String>,
    method: &str,
    recording_id: String,
    as_json: bool,
) -> Result<()> {
    let response = call_service(
        endpoint,
        token,
        method,
        json!({
            "recording_id": { "id": recording_id },
        }),
    )?;
    print_rpc_response(response, as_json)
}

fn call_service(
    endpoint: Option<SocketAddr>,
    token: Option<String>,
    method: &str,
    params: serde_json::Value,
) -> Result<dbgatlas_service::JsonRpcResponse> {
    let connection = resolve_client_connection(endpoint, token)?;
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: method.to_string(),
        params: Some(params),
    };
    Ok(invoke_http_json_rpc(
        connection.endpoint,
        &connection.token,
        &request,
    )?)
}

fn print_rpc_response(response: dbgatlas_service::JsonRpcResponse, as_json: bool) -> Result<()> {
    if as_json {
        print_json(serde_json::to_value(response)?)?;
        return Ok(());
    }

    if let Some(error) = response.error {
        anyhow::bail!("{} ({})", error.message, error.code);
    }
    print_json(response.result.unwrap_or_else(|| json!(null)))
}

struct ClientConnection {
    endpoint: SocketAddr,
    token: String,
}

fn resolve_client_connection(
    endpoint: Option<SocketAddr>,
    token: Option<String>,
) -> Result<ClientConnection> {
    if let (Some(endpoint), Some(token)) = (endpoint.as_ref(), token.as_ref()) {
        return Ok(ClientConnection {
            endpoint: *endpoint,
            token: token.clone(),
        });
    }
    let installed = installed_client_config()?;
    let dev = ServiceConfig::dev_default();
    Ok(ClientConnection {
        endpoint: endpoint
            .or_else(|| installed.as_ref().map(|config| config.bind))
            .unwrap_or(dev.bind),
        token: token
            .or_else(|| installed.map(|config| config.bearer_token))
            .unwrap_or(dev.bearer_token),
    })
}

fn print_service_command_result(
    result: dbgatlas_service::WindowsServiceCommandResult,
    as_json: bool,
) -> Result<()> {
    if as_json {
        return print_json(serde_json::to_value(result)?);
    }
    println!("service: {}", result.service_name);
    println!("status: {}", result.status);
    if let Some(endpoint) = result.endpoint {
        println!("rpc endpoint: http://{endpoint}/rpc");
        println!("mcp endpoint: http://{endpoint}/mcp");
    }
    println!("binary: {}", result.installed_binary.display());
    println!("config: {}", result.config_path.display());
    println!("token file: {}", result.token_file.display());
    println!("log dir: {}", result.log_dir.display());
    Ok(())
}

fn run_native(command: NativeCommand, as_json: bool) -> Result<()> {
    match command {
        NativeCommand::Version => {
            let version = dbgatlas_dbgeng::native_version()?;
            if as_json {
                print_json(serde_json::to_value(version)?)?;
            } else {
                println!(
                    "native ABI version: {}.{}.{}",
                    version.abi_major, version.abi_minor, version.abi_patch
                );
            }
        }
    }
    Ok(())
}

fn print_json(value: serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()?.join(path))
}
