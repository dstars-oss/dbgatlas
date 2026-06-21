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
    // check-fast 只覆盖不依赖 native adapter/linker 的 Rust-only 快速回归。
    // CLI/service/native adapter 的端到端验证仍需单独跑 workspace tests 或 release build。
    eprintln!(
        "running fast Rust-only checks for packages: {}",
        FAST_TEST_PACKAGES.join(", ")
    );
    let mut command = Command::new("cargo");
    command.arg("test");
    for package in FAST_TEST_PACKAGES {
        command.arg("-p").arg(package);
    }

    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("fast checks failed with status {status}");
            eprintln!(
                "next diagnostic step: run `cargo test --workspace` or the release build script for native/service coverage"
            );
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
