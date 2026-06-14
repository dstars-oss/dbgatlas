use anyhow::Result;
use clap::{Parser, Subcommand};
use dbgatlas_debug::DebugTarget;
use dbgatlas_service::{
    JsonRpcRequest, ServiceConfig, ServiceHost, invoke_http_json_rpc, run_http_service,
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
}

#[derive(Subcommand)]
enum ServiceCommand {
    Run {
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    Health {
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    Info {
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    Install,
    Start,
    Stop,
    Status,
    Uninstall,
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
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    Modules {
        session_id: String,
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    Threads {
        session_id: String,
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    Stack {
        session_id: String,
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    ReadMemory {
        session_id: String,
        #[arg(long)]
        address: String,
        #[arg(long)]
        length: u64,
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    AddSymbols {
        session_id: String,
        symbol_path: String,
        #[arg(long)]
        reload: bool,
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
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
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    Close {
        session_id: String,
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
    },
    Kill {
        session_id: String,
        #[arg(long, default_value = "127.0.0.1:7331")]
        endpoint: SocketAddr,
        #[arg(long, default_value = "dev-token")]
        token: String,
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
    }
    Ok(())
}

fn run_service(command: ServiceCommand, as_json: bool) -> Result<()> {
    match command {
        ServiceCommand::Run { bind, token } => {
            let config = ServiceConfig {
                bind,
                bearer_token: token,
            };
            if !as_json {
                println!("DbgAtlas service listening on http://{}/rpc", config.bind);
            }
            run_http_service(config, ServiceHost::with_process_workers()?)?;
        }
        ServiceCommand::Health { endpoint, token } => {
            let response = call_service(endpoint, &token, "service.health", json!({}))?;
            print_rpc_response(response, as_json)?;
        }
        ServiceCommand::Info { endpoint, token } => {
            let response = call_service(endpoint, &token, "service.info", json!({}))?;
            print_rpc_response(response, as_json)?;
        }
        ServiceCommand::Install
        | ServiceCommand::Start
        | ServiceCommand::Stop
        | ServiceCommand::Status
        | ServiceCommand::Uninstall => {
            anyhow::bail!(
                "Windows service control commands are reserved in the CLI surface but not implemented yet; use `dbgatlas service run` for MVP service dev mode"
            );
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
                &token,
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
        } => call_session_tool(endpoint, &token, "debug.modules", session_id, as_json),
        DebugCommand::Threads {
            session_id,
            endpoint,
            token,
        } => call_session_tool(endpoint, &token, "debug.threads", session_id, as_json),
        DebugCommand::Stack {
            session_id,
            endpoint,
            token,
        } => call_session_tool(endpoint, &token, "debug.stack", session_id, as_json),
        DebugCommand::ReadMemory {
            session_id,
            address,
            length,
            endpoint,
            token,
        } => {
            let response = call_service(
                endpoint,
                &token,
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
                &token,
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
                &token,
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
        } => call_session_tool(endpoint, &token, "debug.session.close", session_id, as_json),
        DebugSessionCommand::Kill {
            session_id,
            endpoint,
            token,
        } => call_session_tool(endpoint, &token, "debug.session.kill", session_id, as_json),
    }
}

fn call_session_tool(
    endpoint: SocketAddr,
    token: &str,
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

fn call_service(
    endpoint: SocketAddr,
    token: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<dbgatlas_service::JsonRpcResponse> {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: method.to_string(),
        params: Some(params),
    };
    Ok(invoke_http_json_rpc(endpoint, token, &request)?)
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
