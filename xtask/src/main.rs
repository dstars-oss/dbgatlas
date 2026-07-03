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
        Some("check") => run_checks(),
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

fn run_checks() -> ExitCode {
    let steps: &[(&str, &[&str])] = &[
        ("cargo", &["fmt", "--check"]),
        (
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        ("cargo", &["test", "--workspace"]),
    ];

    for (program, args) in steps {
        eprintln!("running `{program} {}`", args.join(" "));
        if let Err(status) = run_command(program, args) {
            eprintln!("check step failed: `{program} {}`", args.join(" "));
            return status;
        }
    }

    ExitCode::SUCCESS
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

fn run_command(program: &str, args: &[&str]) -> Result<(), ExitCode> {
    match Command::new(program).args(args).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => {
            eprintln!("command exited with status {status}");
            Err(ExitCode::FAILURE)
        }
        Err(error) => {
            eprintln!("failed to run {program}: {error}");
            Err(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("DbgAtlas xtask commands:");
    println!("  check         Run fmt, clippy, and full workspace tests");
    println!("  check-fast    Run Rust-only tests that do not build native adapter crates");
}
