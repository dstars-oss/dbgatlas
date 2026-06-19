use std::env;
use std::process::{Command, ExitCode};

const FAST_TEST_PACKAGES: &[&str] = &[
    "dbgatlas-model",
    "dbgatlas-workspace",
    "dbgatlas-adapter",
    "dbgatlas-core",
    "dbgatlas-debug",
    "dbgatlas-recording",
    "dbgatlas-runtime",
    "dbgatlas-worker-protocol",
];

fn main() -> ExitCode {
    match env::args().nth(1).as_deref() {
        Some("check-fast") => run_fast_checks(),
        Some("-h" | "--help") | None => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprintln!("unknown xtask command `{command}`");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn run_fast_checks() -> ExitCode {
    let mut command = Command::new("cargo");
    command.arg("test");
    for package in FAST_TEST_PACKAGES {
        command.arg("-p").arg(package);
    }

    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("fast checks failed with status {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("failed to run cargo test: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!("DbgAtlas xtask commands:");
    println!("  check-fast    Run Rust-only tests that do not build native adapter crates");
}
