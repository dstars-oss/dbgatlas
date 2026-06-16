use dbgatlas_service::{ServiceConfig, ServiceHost, ServiceShutdown, run_http_service_until};
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::process::Command;
use std::thread;
use std::time::Duration;

#[test]
fn cli_json_debug_workflow_returns_recording_refs() {
    let temp = tempfile::tempdir().unwrap();
    let dump_path = temp.path().join("sample.dmp");
    std::fs::write(&dump_path, b"mock dump").unwrap();
    let endpoint = unused_loopback_endpoint();
    let shutdown = ServiceShutdown::new();
    let server_shutdown = shutdown.clone();
    let server = thread::spawn(move || {
        run_http_service_until(
            ServiceConfig {
                bind: endpoint,
                bearer_token: "test-token".to_string(),
            },
            ServiceHost::with_mock_workers(),
            server_shutdown,
        )
    });
    wait_for_service(endpoint);

    let create = run_dbgatlas([
        "--json",
        "debug",
        "session",
        "create",
        "--project-root",
        temp.path().to_str().unwrap(),
        "--dump",
        dump_path.to_str().unwrap(),
        "--endpoint",
        &endpoint.to_string(),
        "--token",
        "test-token",
    ]);
    let session_id = create["result"]["session_id"]["id"].as_str().unwrap();

    let eval = run_dbgatlas([
        "--json",
        "debug",
        "eval",
        session_id,
        ".echo from-cli",
        "--endpoint",
        &endpoint.to_string(),
        "--token",
        "test-token",
    ]);

    assert_eq!(eval["result"]["operation_status"], "success");
    assert!(eval["result"]["raw_output_ref"].get("id").is_some());
    assert_eq!(eval["result"]["artifact_refs"].as_array().unwrap().len(), 3);

    shutdown.request_stop();
    server.join().unwrap().unwrap();
}

fn run_dbgatlas<const N: usize>(args: [&str; N]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_dbgatlas"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn unused_loopback_endpoint() -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = listener.local_addr().unwrap();
    drop(listener);
    endpoint
}

fn wait_for_service(endpoint: SocketAddr) {
    for _ in 0..50 {
        if TcpListener::bind(endpoint).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "service did not start at {}",
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), endpoint.port())
    );
}
