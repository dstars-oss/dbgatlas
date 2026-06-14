use anyhow::Result;
use clap::{Parser, Subcommand};
use dbgatlas_workspace::{Workspace, WorkspaceInitOptions};
use serde_json::json;
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
