# DbgAtlas

DbgAtlas is a Windows investigation platform for debugging, reverse engineering, event recording, and reproducible problem analysis.

This repository contains the tool implementation. Real investigation data belongs in an explicit analysis workspace, not in hidden repository state.

MVP 0 through MVP 1 contain:

- A Rust workspace split into `model`, `workspace`, `adapter`, `core`, `cli`, and a minimal `dbgeng` native wrapper.
- Debug session lifecycle, per-session worker supervision, named-pipe worker transport, and a minimal real DbgEng loop.
- Runtime configuration types in `dbgatlas-runtime`, kept separate from workspace manifests.
- A visible analysis workspace layout with `artifacts/`, `analysis/`, and optional `inputs/`.
- Controlled artifact helpers for debug sessions and reserved recording/reverse artifact layouts.
- A C++20 DbgEng native DLL with adapter-specific C ABI for open dump, attach process, raw command execution, session symbol path append, and virtual memory reads.
- CLI commands for workspace initialization, workspace inspection, service dev mode, and debug session workflows.
- Windows Service lifecycle commands that install an isolated runtime payload under `%ProgramData%\DbgAtlas\bin\`, machine config under `etc\`, and service logs under `var\log\`.

Useful commands:

```powershell
cargo metadata --format-version 1
cargo test --workspace
cargo run -p dbgatlas-cli -- workspace init .\scratch-workspace --with-inputs
cargo run -p dbgatlas-cli -- workspace info .\scratch-workspace
cargo run -p dbgatlas-cli -- native version
cargo run -p dbgatlas-cli -- service run --bind 127.0.0.1:7331 --token dev-token
.\script\build-release-install.ps1
.\script\build-release-install.ps1 -BuildOnly
dbgatlas service install
dbgatlas service start
dbgatlas service health
dbgatlas service status --json
dbgatlas service stop
dbgatlas service uninstall
cargo run -p dbgatlas-cli -- debug session create --project-root .\scratch-project --dump .\sample.dmp
cargo run -p dbgatlas-cli -- debug eval <session-id> ".echo hello"
cargo run -p dbgatlas-cli -- debug modules <session-id>
cargo run -p dbgatlas-cli -- debug threads <session-id>
cargo run -p dbgatlas-cli -- debug stack <session-id>
cargo run -p dbgatlas-cli -- debug add-symbols <session-id> "srv*C:\symbols*https://msdl.microsoft.com/download/symbols" --reload
cargo run -p dbgatlas-cli -- debug read-memory <session-id> --address 0x1000 --length 64
cargo run -p dbgatlas-cli -- debug session close <session-id>
```

The installed service can update its own runtime payload through JSON-RPC or
MCP by calling `service.update` with a built payload directory:

```json
{
  "source_dir": "D:\\Repos\\Project\\dbgatlas\\target\\release",
  "restart": true,
  "timeout_ms": 60000
}
```

Codex can connect to the service-hosted HTTP MCP endpoint with a bearer token
kept in the local environment:

```toml
[mcp_servers.dbgatlas]
url = "http://127.0.0.1:7331/mcp"
bearer_token_env_var = "DBGATLAS_TOKEN"
enabled = true
startup_timeout_sec = 10
tool_timeout_sec = 120
```

For the development service command above, start Codex with
`DBGATLAS_TOKEN=dev-token`.
