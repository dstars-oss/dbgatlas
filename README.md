# DbgAtlas

DbgAtlas is an AI-oriented debugging platform. AI clients use native debugger commands with session management, state, and recorded outputs. Windows debugging and recording are implemented first; environment/session/process restructuring, remote backends, persistent searchable history, Linux, and a management UI follow [PLAN.md](PLAN.md).

The [architecture specification](docs/architecture.md) and [environment/session model](docs/environment-session-model.md) define the target design. The next iteration restructures environments, sessions, and process ownership on the local path; remote transport and the later AI execution interface are separate iterations. See the [implementation baseline](docs/roadmap.md) for current code paths and limitations.

IDA static analysis belongs to external tools. DbgAtlas provides debugger-native symbols, types, disassembly, and memory inspection.

This repository contains the tool implementation. Real investigation data belongs in an explicit analysis workspace, not in hidden repository state.

Current implementation includes:

- A `0.3.0` service API with one method registry driving JSON-RPC dispatch and MCP tool schemas.
- A Rust workspace split into `model`, `workspace`, `adapter`, `core`, `cli`, and a minimal `dbgeng` native wrapper.
- Debug session lifecycle, per-session worker supervision, named-pipe worker transport, and a minimal real DbgEng loop.
- Runtime configuration types in `dbgatlas-runtime`, kept separate from workspace manifests.
- A visible analysis workspace layout with `artifacts/`, `analysis/`, and optional `inputs/`.
- Controlled artifact helpers for debug and recording sessions.
- A C++20 DbgEng native DLL with adapter-specific C ABI for opening DbgEng files (`.dmp`, `.run`), attach process, raw command execution, session symbol path append, and virtual memory reads.
- CLI commands for workspace initialization, workspace inspection, service dev mode, and debug session workflows.
- RPC/MCP project fact queries through `artifact.list`, `operation.list`, and `audit.list_commands`, plus the complete ETW recording lifecycle.
- Windows Service lifecycle commands that install or use an isolated runtime payload under `%ProgramFiles%\DbgAtlas\bin\`, machine config under `etc\`, and service logs under `var\log\`.

Useful commands:

```powershell
cargo metadata --format-version 1
cargo run -p xtask -- check
cargo test --workspace
cargo run -p dbgatlas-cli -- workspace init .\temp\scratch-workspace --with-inputs
cargo run -p dbgatlas-cli -- workspace info .\temp\scratch-workspace
cargo run -p dbgatlas-cli -- native version
cargo run -p dbgatlas-cli -- service run --bind 127.0.0.1:7331
.\script\build-release-install.ps1
.\script\build-release-install.ps1 -BuildOnly
.\script\build-msi.ps1
.\script\uninstall.ps1
dbgatlas service install --payload-mode copy --payload-dir .\target\release
dbgatlas service start
dbgatlas service health
dbgatlas service status --json
dbgatlas service stop
dbgatlas service uninstall --purge
cargo run -p dbgatlas-cli -- debug session create --project-root .\temp\scratch-project --file .\sample.dmp
cargo run -p dbgatlas-cli -- debug session create --project-root .\temp\scratch-project --file .\trace.run
cargo run -p dbgatlas-cli -- debug eval <session-id> ".echo hello"
cargo run -p dbgatlas-cli -- debug modules <session-id>
cargo run -p dbgatlas-cli -- debug threads <session-id>
cargo run -p dbgatlas-cli -- debug stack <session-id>
cargo run -p dbgatlas-cli -- debug add-symbols <session-id> "srv*C:\symbols*https://msdl.microsoft.com/download/symbols" --reload
cargo run -p dbgatlas-cli -- debug read-memory <session-id> --address 0x1000 --length 64
cargo run -p dbgatlas-cli -- debug session close <session-id>
cargo run -p dbgatlas-cli -- recording ttd --project-root .\temp\scratch-project --launch C:\Windows\System32\cmd.exe --start-directory C:\Windows\System32 --timeout-ms 30000 --accept-eula -- /C exit 0
cargo run -p dbgatlas-cli -- recording ttd --project-root .\temp\scratch-project --attach <pid> --timeout-ms 30000 --accept-eula --worker-identity active_interactive_user
```

For MSI installation and Codex / Claude Code MCP client configuration, see
[`docs/install-configuration.md`](docs/install-configuration.md).

The installed service can update its own runtime payload through JSON-RPC or
MCP by calling `service.update` with a built payload directory:

```json
{
  "source_dir": "D:\\Repos\\Project\\dbgatlas\\target\\release",
  "restart": true,
  "timeout_ms": 60000
}
```

Codex connects directly to the loopback HTTP MCP endpoint without authentication:

```toml
[mcp_servers.dbgatlas]
url = "http://127.0.0.1:7331/mcp"
enabled = true
startup_timeout_sec = 10
tool_timeout_sec = 120
```

The service trusts local callers and retains loopback binding and browser `Origin` checks.
