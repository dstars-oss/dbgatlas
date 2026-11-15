//! Development-only real-worker harness with an explicit DbgEng runtime.
//! Used by script/test-worker-interaction.py; never changes installed configuration.
use dbgatlas_service::{ServiceCapabilities, ServiceConfig, ServiceHost, run_http_service};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let value = |key: &str| -> Result<String, Box<dyn std::error::Error>> {
        let index = args
            .iter()
            .position(|arg| arg == key)
            .ok_or("missing argument")?;
        Ok(args.get(index + 1).ok_or("missing value")?.clone())
    };
    let host = ServiceHost::with_process_worker_exe(PathBuf::from(value("--worker")?))?
        .with_capabilities(ServiceCapabilities {
            dbgeng_dirs: vec![PathBuf::from(value("--dbgeng")?)],
            ttd_dir: None,
        });
    run_http_service(
        ServiceConfig {
            bind: value("--bind")?.parse()?,
        },
        host,
    )?;
    Ok(())
}
