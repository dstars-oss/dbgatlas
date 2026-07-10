use dbgatlas_service::{
    JsonRpcRequest, JsonRpcResponse, MockWorkerSupervisor, ProcessWorkerSupervisor, ServiceConfig,
    ServiceHost, ServiceInstallPaths, ServiceShutdown, WindowsServiceInstallOptions,
    WorkerCreateRequest, WorkerHandle, WorkerIdentity, WorkerSupervisor, invoke_http_json_rpc,
    run_http_service, run_http_service_until,
};

#[test]
fn crate_root_service_api_remains_importable() {
    let _ = std::mem::size_of::<JsonRpcRequest>();
    let _ = std::mem::size_of::<JsonRpcResponse>();
    let _ = std::mem::size_of::<MockWorkerSupervisor>();
    let _ = std::mem::size_of::<ProcessWorkerSupervisor>();
    let _ = std::mem::size_of::<ServiceConfig>();
    let _ = std::mem::size_of::<ServiceHost>();
    let _ = std::mem::size_of::<ServiceInstallPaths>();
    let _ = std::mem::size_of::<ServiceShutdown>();
    let _ = std::mem::size_of::<WindowsServiceInstallOptions>();
    let _ = std::mem::size_of::<WorkerCreateRequest>();
    let _ = std::mem::size_of::<WorkerHandle>();
    let _ = std::mem::size_of::<WorkerIdentity>();
    let _ = std::mem::size_of::<&dyn WorkerSupervisor>();
    let _ = invoke_http_json_rpc;
    let _ = run_http_service;
    let _ = run_http_service_until;
}
