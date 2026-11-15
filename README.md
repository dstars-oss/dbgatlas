# DbgAtlas

DbgAtlas is an AI-oriented debugging platform. AI clients use native debugger commands with session management, state, and recorded outputs. Windows debugging and recording use dedicated session workers and service-owned executions; remote backends, persistent searchable history, Linux, and a management UI follow [PLAN.md](PLAN.md).

The [architecture specification](docs/architecture.md) and [implementation baseline](docs/roadmap.md) describe the design and current code paths. Iteration 3 implements Session + Execution, bounded waits, reliable interruption, cursor-based output reads and Artifact provenance. MCP version negotiation, structured results and canonical CLI commands are implemented and locally validated with the real Codex client. Installed-service acceptance remains pending; see the [interaction contract](docs/execution-interaction.md) and [validation scope](docs/iteration-3-validation.md).

IDA static analysis belongs to external tools. DbgAtlas provides debugger-native symbols, types, disassembly, and memory inspection.

This repository contains the tool implementation. Real investigation data belongs in an explicit analysis workspace, not in hidden repository state.

Current implementation includes:

- A `0.4.0` service API with one method registry driving JSON-RPC dispatch and MCP tool schemas.
- A Rust workspace split into `model`, `workspace`, `adapter`, `core`, `cli`, and a minimal `dbgeng` native wrapper.
- Session/Execution API with terminal summaries, immutable execution context and versioned JSONL updates. Dedicated workers for DbgEng, ETW and TTD recording, using local named-pipe protocol v9; internal Operation records remain compatible.
- Independent `session.interrupt` and cooperative command cancellation, with epoch validation, native events and raw output spooling. Action waits default to 1000 ms (maximum 30000); expiry does not cancel or kill. Explicit legacy debug `timeout_ms` is rejected with migration guidance to `wait_ms`.
- Runtime configuration types in `dbgatlas-runtime`, kept separate from workspace manifests.
- A visible analysis workspace layout with `artifacts/`, `analysis/`, and optional `inputs/`.
- Controlled artifact helpers for debug and recording sessions.
- A C++20 DbgEng native DLL with adapter-specific C ABI for opening DbgEng files (`.dmp`, `.run`), attach process, raw command execution, session symbol path append, and virtual memory reads.
- CLI `session` and `execution` commands reuse the service API; raw commands are sent unchanged, without automatic retries or polling. `--json` preserves the response envelope and failures return a nonzero exit code.
- Stateless HTTP MCP supports 2024-11-05, 2025-03-26 and 2025-06-18. The newest supported version returns one structured body plus a compact text summary; older versions return full JSON text. The default inventory contains 25 tools, with compatibility aliases hidden.
- `session.open/exec/get/list/interrupt/close/kill/read/help` and `execution.get/list/cancel/read`; legacy debug/operation aliases share the same execution IDs. `operation.stream` now returns bounded output items and a cursor.
- RPC/MCP project fact queries through `artifact.list`, `execution.list`, and `audit.list_commands`, plus the complete ETW recording lifecycle.
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
cargo run -p dbgatlas-cli -- session open --project-root .\temp\scratch-project --file .\sample.dmp
cargo run -p dbgatlas-cli -- session open --project-root .\temp\scratch-project --file .\trace.run
cargo run -p dbgatlas-cli -- session open --project-root .\temp\scratch-project --attach <pid> --attach-mode invasive
cargo run -p dbgatlas-cli -- session exec <session-id> ".echo hello" --wait-ms 1000
cargo run -p dbgatlas-cli -- execution get <execution-id> --wait-ms 30000
cargo run -p dbgatlas-cli -- execution read <execution-id> --max-bytes 8192
cargo run -p dbgatlas-cli -- session exec <session-id> "g"
cargo run -p dbgatlas-cli -- session interrupt <session-id>
cargo run -p dbgatlas-cli -- session help execution --session-id <session-id>
cargo run -p dbgatlas-cli -- debug read-memory <session-id> --address 0x1000 --length 64
cargo run -p dbgatlas-cli -- session close <session-id>
cargo run -p dbgatlas-cli -- recording ttd --project-root .\temp\scratch-project --launch C:\Windows\System32\cmd.exe --start-directory C:\Windows\System32 --timeout-ms 30000 --accept-eula -- /C exit 0
cargo run -p dbgatlas-cli -- recording ttd --project-root .\temp\scratch-project --attach <pid> --timeout-ms 30000 --accept-eula --worker-identity active_interactive_user
```

Use IDs returned by the preceding call. An expired wait returns the accepted Execution, which can be observed and read later. `execution cancel` requests cooperative command cancellation; `session interrupt` stops a running target. Attach defaults to noninvasive observation. Use `--cursor` from a read response to continue reading and `session get` to inspect an opening session before submitting commands. Legacy `debug` commands remain accepted but are hidden from the primary help.

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
