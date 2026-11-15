use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use dbgatlas_debug::DebugTarget;
use dbgatlas_recording::{
    DEFAULT_CPU_SAMPLING_INTERVAL_US, DEFAULT_CPU_TRACE_SIZE_MB, RecordingPreset, RecordingTarget,
    TtdRecordMode, TtdRecordingOptions, TtdReplayCpuSupport, TtdTarget,
};
use dbgatlas_service::{
    DEFAULT_SERVICE_UPDATE_TIMEOUT_MS, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    ServiceConfig, ServiceHost, TtdCommandHelperOptions, WindowsServiceApplyUpdateOptions,
    WindowsServiceControlOptions, WindowsServiceDoctorOptions, WindowsServiceInstallOptions,
    WindowsServicePayloadMode, WindowsServiceRunOptions, WindowsServiceUninstallOptions,
    apply_windows_service_update, format_doctor_report, install_windows_service,
    installed_client_config, invoke_http_json_rpc, run_http_service, run_ttd_command_helper,
    run_windows_service_dispatcher, run_windows_service_doctor, start_windows_service,
    status_windows_service, stop_windows_service, uninstall_windows_service,
};
use dbgatlas_workspace::{Workspace, WorkspaceInitOptions};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
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
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Rpc {
        #[command(subcommand)]
        command: RpcCommand,
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
enum SessionCommand {
    Get {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    List {
        #[arg(long, default_value="debug", value_parser=["debug","recording","all"])]
        kind: String,
        #[arg(long)]
        include_terminal: bool,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
}
fn run_session(command: SessionCommand, as_json: bool) -> Result<()> {
    let response = match command {
        SessionCommand::Get {
            session_id,
            endpoint,
        } => call_service(endpoint, "session.get", json!({"session_id":session_id}))?,
        SessionCommand::List {
            kind,
            include_terminal,
            endpoint,
        } => call_service(
            endpoint,
            "session.list",
            json!({"kind":kind,"include_terminal":include_terminal}),
        )?,
    };
    print_rpc_response(response, as_json)
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ServicePayloadModeArg {
    Copy,
    UseExisting,
}

impl From<ServicePayloadModeArg> for WindowsServicePayloadMode {
    fn from(value: ServicePayloadModeArg) -> Self {
        match value {
            ServicePayloadModeArg::Copy => WindowsServicePayloadMode::Copy,
            ServicePayloadModeArg::UseExisting => WindowsServicePayloadMode::UseExisting,
        }
    }
}

#[derive(Subcommand)]
enum ServiceCommand {
    Run {
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: SocketAddr,
        #[arg(long, hide = true)]
        windows_service: bool,
        #[arg(long, hide = true)]
        install_root: Option<PathBuf>,
        #[arg(long, hide = true)]
        config: Option<PathBuf>,
    },
    Health {
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Info {
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Install {
        #[arg(long)]
        install_root: Option<PathBuf>,
        #[arg(long)]
        payload_dir: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "copy")]
        payload_mode: ServicePayloadModeArg,
        #[arg(long, default_value = "127.0.0.1:7331")]
        bind: SocketAddr,
        #[arg(long)]
        force: bool,
    },
    Start {
        #[arg(long)]
        install_root: Option<PathBuf>,
    },
    Stop {
        #[arg(long)]
        install_root: Option<PathBuf>,
    },
    Status {
        #[arg(long)]
        install_root: Option<PathBuf>,
    },
    Doctor {
        #[arg(long)]
        install_root: Option<PathBuf>,
        #[arg(long)]
        report_path: Option<PathBuf>,
        #[arg(long)]
        msi_summary_path: Option<PathBuf>,
    },
    Uninstall {
        #[arg(long)]
        install_root: Option<PathBuf>,
        #[arg(long)]
        purge: bool,
    },
    #[command(hide = true)]
    ApplyUpdate {
        #[arg(long)]
        install_root: Option<PathBuf>,
        #[arg(long)]
        source_dir: PathBuf,
        #[arg(long, default_value_t = DEFAULT_SERVICE_UPDATE_TIMEOUT_MS)]
        timeout_ms: u64,
        #[arg(long)]
        no_restart: bool,
    },
    #[command(hide = true)]
    RunTtdCommand {
        #[arg(long)]
        executable: PathBuf,
        #[arg(long)]
        current_dir: Option<PathBuf>,
        #[arg(long)]
        stdout_path: PathBuf,
        #[arg(long)]
        stderr_path: PathBuf,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

#[derive(Subcommand)]
enum RpcCommand {
    Local {
        #[arg(long)]
        requests_jsonl: PathBuf,
        #[arg(long)]
        worker_exe: Option<PathBuf>,
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
    },
    Modules {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Threads {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Stack {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    ReadMemory {
        session_id: String,
        #[arg(long)]
        address: String,
        #[arg(long)]
        length: u64,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    AddSymbols {
        session_id: String,
        symbol_path: String,
        #[arg(long)]
        reload: bool,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
}

#[derive(Subcommand)]
enum DebugSessionCommand {
    Create {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        attach: Option<u32>,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Close {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Kill {
        session_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
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
        #[arg(long = "preset")]
        presets: Vec<String>,
        #[arg(long)]
        cpu_interval_us: Option<u32>,
        #[arg(long)]
        cpu_max_trace_size_mb: Option<u32>,
        #[arg(last = true)]
        args: Vec<String>,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Ttd {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        launch: Option<PathBuf>,
        #[arg(long)]
        start_directory: Option<PathBuf>,
        #[arg(long)]
        attach: Option<u32>,
        #[arg(long)]
        monitor: Option<PathBuf>,
        #[arg(long)]
        cmd_line_filter: Option<String>,
        #[arg(long)]
        timeout_ms: u64,
        #[arg(long)]
        accept_eula: bool,
        #[arg(long)]
        children: bool,
        #[arg(long)]
        ring: bool,
        #[arg(long, default_value_t = 2048)]
        max_file_mb: u32,
        #[arg(long = "module")]
        modules: Vec<String>,
        #[arg(long, default_value = "automatic")]
        record_mode: String,
        #[arg(long, default_value = "default")]
        replay_cpu_support: String,
        #[arg(long, default_value = "default")]
        worker_identity: String,
        #[arg(long)]
        show_ui: bool,
        #[arg(last = true)]
        args: Vec<String>,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Status {
        recording_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Stop {
        recording_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Cancel {
        recording_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
    },
    Kill {
        recording_id: String,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
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
        Commands::Session { command } => run_session(command, cli.json),
        Commands::Workspace { command } => run_workspace(command, cli.json),
        Commands::Service { command } => run_service(command, cli.json),
        Commands::Rpc { command } => run_rpc(command),
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

fn run_rpc(command: RpcCommand) -> Result<()> {
    match command {
        RpcCommand::Local {
            requests_jsonl,
            worker_exe,
        } => run_local_rpc(requests_jsonl, worker_exe),
    }
}

fn run_local_rpc(requests_jsonl: PathBuf, worker_exe: Option<PathBuf>) -> Result<()> {
    let worker_exe = worker_exe.map(absolute_path).transpose()?;
    let host = match worker_exe {
        Some(worker_exe) => ServiceHost::with_process_worker_exe(worker_exe)?,
        None => ServiceHost::with_process_workers()?,
    };
    let file = File::open(&requests_jsonl)
        .with_context(|| format!("failed to open {}", requests_jsonl.display()))?;
    let reader = BufReader::new(file);
    let mut responses = HashMap::<String, Value>::new();

    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = line.with_context(|| {
            format!(
                "failed to read JSON-RPC request at {}:{line_number}",
                requests_jsonl.display()
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }

        let mut request: JsonRpcRequest = serde_json::from_str(&line).with_context(|| {
            format!(
                "failed to parse JSON-RPC request at {}:{line_number}",
                requests_jsonl.display()
            )
        })?;
        let response = if request.jsonrpc != "2.0" {
            local_rpc_error_response(request.id.clone(), "jsonrpc must be `2.0`".to_string())
        } else {
            match resolve_request_refs(&mut request, &responses) {
                Ok(()) => host.handle_rpc(request),
                Err(message) => local_rpc_error_response(request.id.clone(), message),
            }
        };
        let response_value = serde_json::to_value(&response)?;
        if let Some(id) = response.id.as_ref().map(response_id_key) {
            responses.insert(id, response_value.clone());
        }
        println!("{}", serde_json::to_string(&response_value)?);
    }

    Ok(())
}

fn run_service(command: ServiceCommand, as_json: bool) -> Result<()> {
    match command {
        ServiceCommand::Run {
            bind,
            windows_service,
            install_root,
            config,
        } => {
            if windows_service {
                let config_path = config.ok_or_else(|| {
                    anyhow::anyhow!("--config is required with --windows-service")
                })?;
                let install_root = install_root
                    .or_else(|| install_root_from_config_path(&config_path))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "--install-root is required with --windows-service when --config is not under <install-root>\\etc"
                        )
                    })?;
                return Ok(run_windows_service_dispatcher(WindowsServiceRunOptions {
                    install_root,
                    config_path,
                })?);
            }
            let config = ServiceConfig { bind };
            if !as_json {
                println!(
                    "DbgAtlas service RPC listening on http://{}/rpc",
                    config.bind
                );
                println!("DbgAtlas MCP listening on http://{}/mcp", config.bind);
            }
            run_http_service(config, ServiceHost::with_process_workers()?)?;
        }
        ServiceCommand::Health { endpoint } => {
            let response = call_service(endpoint, "service.health", json!({}))?;
            print_rpc_response(response, as_json)?;
        }
        ServiceCommand::Info { endpoint } => {
            let response = call_service(endpoint, "service.info", json!({}))?;
            print_rpc_response(response, as_json)?;
        }
        ServiceCommand::Install {
            install_root,
            payload_dir,
            payload_mode,
            bind,
            force,
        } => {
            let result = install_windows_service(WindowsServiceInstallOptions {
                install_root,
                payload_dir,
                payload_mode: payload_mode.into(),
                bind,
                force,
            })?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::Start { install_root } => {
            let result = start_windows_service(WindowsServiceControlOptions { install_root })?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::Stop { install_root } => {
            let result = stop_windows_service(WindowsServiceControlOptions { install_root })?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::Status { install_root } => {
            let result = status_windows_service(WindowsServiceControlOptions { install_root })?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::Doctor {
            install_root,
            report_path,
            msi_summary_path,
        } => {
            let report = run_windows_service_doctor(WindowsServiceDoctorOptions {
                install_root,
                report_path,
                msi_summary_path,
            })?;
            print_doctor_command_result(report, as_json)?;
        }
        ServiceCommand::Uninstall {
            install_root,
            purge,
        } => {
            let result = uninstall_windows_service(WindowsServiceUninstallOptions {
                install_root,
                purge,
            })?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::ApplyUpdate {
            install_root,
            source_dir,
            timeout_ms,
            no_restart,
        } => {
            let result = apply_windows_service_update(WindowsServiceApplyUpdateOptions {
                install_root,
                source_dir,
                restart: !no_restart,
                timeout_ms,
            })?;
            print_service_command_result(result, as_json)?;
        }
        ServiceCommand::RunTtdCommand {
            executable,
            current_dir,
            stdout_path,
            stderr_path,
            args,
        } => {
            let exit_code = run_ttd_command_helper(TtdCommandHelperOptions {
                executable,
                current_dir,
                args,
                stdout_path,
                stderr_path,
            });
            std::process::exit(exit_code);
        }
    }
    Ok(())
}

fn install_root_from_config_path(config_path: &std::path::Path) -> Option<PathBuf> {
    config_path.parent()?.parent().map(PathBuf::from)
}

fn run_debug(command: DebugCommand, as_json: bool) -> Result<()> {
    match command {
        DebugCommand::Session { command } => run_debug_session(command, as_json),
        DebugCommand::Eval {
            session_id,
            command,
            endpoint,
        } => {
            let response = call_service(
                endpoint,
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
        } => call_session_tool(endpoint, "debug.modules", session_id, as_json),
        DebugCommand::Threads {
            session_id,
            endpoint,
        } => call_session_tool(endpoint, "debug.threads", session_id, as_json),
        DebugCommand::Stack {
            session_id,
            endpoint,
        } => call_session_tool(endpoint, "debug.stack", session_id, as_json),
        DebugCommand::ReadMemory {
            session_id,
            address,
            length,
            endpoint,
        } => {
            let response = call_service(
                endpoint,
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
        } => {
            let response = call_service(
                endpoint,
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
            file,
            attach,
            endpoint,
        } => {
            let project_root = absolute_path(project_root)?;
            let target = match (file, attach) {
                (Some(path), None) => serde_json::to_value(DebugTarget::File {
                    path: std::fs::canonicalize(path)?,
                })?,
                (None, Some(pid)) => serde_json::to_value(DebugTarget::Attach {
                    pid,
                    mode: dbgatlas_debug::AttachMode::Noninvasive,
                })?,
                _ => anyhow::bail!("provide exactly one of --file or --attach"),
            };
            let response = call_service(
                endpoint,
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
        } => call_session_tool(endpoint, "debug.session.close", session_id, as_json),
        DebugSessionCommand::Kill {
            session_id,
            endpoint,
        } => call_session_tool(endpoint, "debug.session.kill", session_id, as_json),
    }
}

fn run_recording(command: RecordingCommand, as_json: bool) -> Result<()> {
    match command {
        RecordingCommand::Start {
            project_root,
            launch,
            attach,
            presets,
            cpu_interval_us,
            cpu_max_trace_size_mb,
            args,
            endpoint,
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
            let mut params = json!({
                "project_root": project_root,
                "target": target,
            });
            if !presets.is_empty() {
                params["presets"] = serde_json::to_value(
                    presets
                        .iter()
                        .map(|preset| parse_recording_preset(preset))
                        .collect::<Result<Vec<_>>>()?,
                )?;
            }
            if cpu_interval_us.is_some() || cpu_max_trace_size_mb.is_some() {
                params["cpu_sampling"] = json!({
                    "interval_us": cpu_interval_us.unwrap_or(DEFAULT_CPU_SAMPLING_INTERVAL_US),
                    "max_trace_size_mb": cpu_max_trace_size_mb.unwrap_or(DEFAULT_CPU_TRACE_SIZE_MB),
                });
            }
            let response = call_service(endpoint, "recording.start", params)?;
            print_rpc_response(response, as_json)
        }
        RecordingCommand::Ttd {
            project_root,
            launch,
            start_directory,
            attach,
            monitor,
            cmd_line_filter,
            timeout_ms,
            accept_eula,
            children,
            ring,
            max_file_mb,
            modules,
            record_mode,
            replay_cpu_support,
            worker_identity,
            show_ui,
            args,
            endpoint,
        } => {
            let project_root = absolute_path(project_root)?;
            let target = match (launch, attach, monitor) {
                (Some(executable), None, None) => serde_json::to_value(TtdTarget::Launch {
                    executable: absolute_path(executable)?,
                    start_directory: start_directory.map(absolute_path).transpose()?,
                    args,
                })?,
                (None, Some(pid), None) => {
                    if start_directory.is_some() {
                        anyhow::bail!("--start-directory can only be used with --launch");
                    }
                    if !args.is_empty() {
                        anyhow::bail!("--attach does not accept launch args");
                    }
                    serde_json::to_value(TtdTarget::Attach { pid })?
                }
                (None, None, Some(program)) => {
                    if start_directory.is_some() {
                        anyhow::bail!("--start-directory can only be used with --launch");
                    }
                    if !args.is_empty() {
                        anyhow::bail!("--monitor does not accept launch args");
                    }
                    serde_json::to_value(TtdTarget::Monitor {
                        program,
                        cmd_line_filter,
                    })?
                }
                _ => anyhow::bail!("provide exactly one of --launch, --attach, or --monitor"),
            };
            let options = TtdRecordingOptions {
                children,
                no_ui: !show_ui,
                accept_eula,
                ring,
                max_file_mb,
                modules,
                record_mode: parse_ttd_record_mode(&record_mode)?,
                replay_cpu_support: parse_ttd_replay_cpu_support(&replay_cpu_support)?,
            };
            let response = call_service(
                endpoint,
                "recording.ttd",
                json!({
                    "project_root": project_root,
                    "target": target,
                    "timeout_ms": timeout_ms,
                    "worker_identity": parse_ttd_worker_identity(&worker_identity)?,
                    "options": options,
                }),
            )?;
            print_rpc_response(response, as_json)
        }
        RecordingCommand::Status {
            recording_id,
            endpoint,
        } => call_recording_tool(endpoint, "recording.status", recording_id, as_json),
        RecordingCommand::Stop {
            recording_id,
            endpoint,
        } => call_recording_tool(endpoint, "recording.stop", recording_id, as_json),
        RecordingCommand::Cancel {
            recording_id,
            endpoint,
        } => call_recording_tool(endpoint, "recording.cancel", recording_id, as_json),
        RecordingCommand::Kill {
            recording_id,
            endpoint,
        } => call_recording_tool(endpoint, "recording.kill", recording_id, as_json),
    }
}

fn call_session_tool(
    endpoint: Option<SocketAddr>,
    method: &str,
    session_id: String,
    as_json: bool,
) -> Result<()> {
    let response = call_service(
        endpoint,
        method,
        json!({
            "session_id": { "id": session_id },
        }),
    )?;
    print_rpc_response(response, as_json)
}

fn call_recording_tool(
    endpoint: Option<SocketAddr>,
    method: &str,
    recording_id: String,
    as_json: bool,
) -> Result<()> {
    let response = call_service(
        endpoint,
        method,
        json!({
            "recording_id": { "id": recording_id },
        }),
    )?;
    print_rpc_response(response, as_json)
}

fn call_service(
    endpoint: Option<SocketAddr>,
    method: &str,
    params: serde_json::Value,
) -> Result<dbgatlas_service::JsonRpcResponse> {
    let connection = resolve_client_connection(endpoint)?;
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: method.to_string(),
        params: Some(params),
    };
    Ok(invoke_http_json_rpc(connection.endpoint, &request)?)
}

fn print_rpc_response(response: dbgatlas_service::JsonRpcResponse, as_json: bool) -> Result<()> {
    if as_json {
        print_json(serde_json::to_value(response)?)?;
        return Ok(());
    }

    if let Some(error) = response.error {
        anyhow::bail!("{} ({})", error.message, error.code);
    }
    print_json(response.result.unwrap_or(Value::Null))
}

struct ClientConnection {
    endpoint: SocketAddr,
}

fn resolve_client_connection(endpoint: Option<SocketAddr>) -> Result<ClientConnection> {
    if let Some(endpoint) = endpoint {
        return Ok(ClientConnection { endpoint });
    }
    // 默认连接顺序：显式参数 > 安装态 etc/runtime.toml > 开发态默认值。
    let config = installed_client_config()?.unwrap_or_else(ServiceConfig::dev_default);
    Ok(ClientConnection {
        endpoint: config.bind,
    })
}

fn parse_recording_preset(value: &str) -> Result<RecordingPreset> {
    match value {
        "process" => Ok(RecordingPreset::Process),
        "thread" => Ok(RecordingPreset::Thread),
        "image" => Ok(RecordingPreset::Image),
        "file" => Ok(RecordingPreset::File),
        "registry" => Ok(RecordingPreset::Registry),
        "network" => Ok(RecordingPreset::Network),
        "cpu" => Ok(RecordingPreset::Cpu),
        _ => anyhow::bail!(
            "unsupported recording preset `{value}`; expected process, thread, image, file, registry, network, or cpu"
        ),
    }
}

fn parse_ttd_record_mode(value: &str) -> Result<TtdRecordMode> {
    match value {
        "automatic" => Ok(TtdRecordMode::Automatic),
        "manual" => Ok(TtdRecordMode::Manual),
        other => anyhow::bail!("unsupported --record-mode `{other}`"),
    }
}

fn parse_ttd_replay_cpu_support(value: &str) -> Result<TtdReplayCpuSupport> {
    match value {
        "default" => Ok(TtdReplayCpuSupport::Default),
        "most_conservative" => Ok(TtdReplayCpuSupport::MostConservative),
        "most_aggressive" => Ok(TtdReplayCpuSupport::MostAggressive),
        "intel_avx_required" => Ok(TtdReplayCpuSupport::IntelAvxRequired),
        "intel_avx2_required" => Ok(TtdReplayCpuSupport::IntelAvx2Required),
        other => anyhow::bail!("unsupported --replay-cpu-support `{other}`"),
    }
}

fn parse_ttd_worker_identity(value: &str) -> Result<&'static str> {
    match value {
        "default" => Ok("default"),
        "active_interactive_user" => Ok("active_interactive_user"),
        other => anyhow::bail!("unsupported --worker-identity `{other}`"),
    }
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
    println!("log dir: {}", result.log_dir.display());
    Ok(())
}

fn print_doctor_command_result(
    report: dbgatlas_service::WindowsServiceDoctorReport,
    as_json: bool,
) -> Result<()> {
    if as_json {
        return print_json(serde_json::to_value(report)?);
    }
    print!("{}", format_doctor_report(&report));
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

fn resolve_request_refs(
    request: &mut JsonRpcRequest,
    responses: &HashMap<String, Value>,
) -> std::result::Result<(), String> {
    if let Some(params) = request.params.as_mut() {
        resolve_response_refs(params, responses)?;
    }
    Ok(())
}

fn resolve_response_refs(
    value: &mut Value,
    responses: &HashMap<String, Value>,
) -> std::result::Result<(), String> {
    match value {
        Value::String(text) => {
            if let Some((id, pointer)) = parse_response_ref(text) {
                let replacement = responses
                    .get(id)
                    .and_then(|response| response.pointer(pointer))
                    .cloned()
                    .ok_or_else(|| {
                        format!("response reference ${id}{pointer} could not be resolved")
                    })?;
                *value = replacement;
            }
        }
        Value::Array(items) => {
            for item in items {
                resolve_response_refs(item, responses)?;
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                resolve_response_refs(value, responses)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_response_ref(value: &str) -> Option<(&str, &str)> {
    let rest = value.strip_prefix('$')?;
    let slash = rest.find('/')?;
    let id = &rest[..slash];
    if id.is_empty() {
        return None;
    }
    Some((id, &rest[slash..]))
}

fn response_id_key(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

fn local_rpc_error_response(id: Option<Value>, message: String) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code: -32602,
            message,
        }),
    }
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()?.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repeatable_cpu_recording_options() {
        let cli = Cli::try_parse_from([
            "dbgatlas",
            "recording",
            "start",
            "--project-root",
            ".",
            "--attach",
            "42",
            "--preset",
            "process",
            "--preset",
            "cpu",
            "--cpu-interval-us",
            "2000",
            "--cpu-max-trace-size-mb",
            "256",
        ])
        .unwrap();
        let Commands::Recording {
            command:
                RecordingCommand::Start {
                    presets,
                    cpu_interval_us,
                    cpu_max_trace_size_mb,
                    ..
                },
        } = cli.command
        else {
            panic!("expected recording start command");
        };
        assert_eq!(presets, ["process", "cpu"]);
        assert_eq!(cpu_interval_us, Some(2000));
        assert_eq!(cpu_max_trace_size_mb, Some(256));
        assert_eq!(parse_recording_preset("cpu").unwrap(), RecordingPreset::Cpu);
        assert!(parse_recording_preset("unknown").is_err());
    }
}
