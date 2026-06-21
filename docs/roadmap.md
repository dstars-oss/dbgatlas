# Roadmap

## Current Baseline

- Cargo workspace and native CMake project.
- Visible analysis workspace initialization, inspection, artifact metadata, operation log, and command audit log.
- Minimal adapter contract with Rust core orchestration.
- Runtime config separated from workspace manifests.
- Domain artifact layout helpers and artifact path validation.
- Native C ABI boundaries for DbgEng, ETW, and IDA adapters with matching Rust `*-sys` crates and safe wrappers.
- Installed Windows service lifecycle, development service mode, JSON-RPC `/rpc`, and HTTP MCP `/mcp`.
- `--json` output for the main CLI workflows.

## Debug Workflow

- Per-session worker supervision and serialized session requests.
- Debug session create/close/cancel/kill.
- Open DbgEng-supported debug files such as `.dmp` and `.run`.
- Attach process.
- Execute raw WinDbg command and capture output.
- Record transcript, events, raw outputs, operations, and artifacts.
- List modules, list threads, get stack, add session symbols, and read memory to artifact files.

## Recording Workflow

- `recording.*` lifecycle for ETW-style process tree recording.
- Launch and attach targets.
- Built-in process/thread/image/file/registry/network presets.
- Filtered ETL, recording metadata, and category event JSONL artifacts.
- TTD one-shot recording under the shared recording namespace.
- Low-level recording materials available as inputs for report, reverse, and future timeline views.

## Reverse Workflow

- IDA database target opening through native IDA adapter dynamic loading.
- Reverse sessions in active interactive user workers.
- Explicit runtime address / module base / IDA image base to IDA function mapping.
- Core Functions exposed through service RPC and MCP tools with stable JSON result shape; inputs and artifacts are described in `docs/reverse-core-functions.md`.
- Reverse workflow outputs, operation records, and artifact metadata are referenceable from `analysis/` notes.

## Next Directions

- Markdown report workflow that cites artifact ids and operation ids.
- Agent-oriented workflows over the service-hosted MCP endpoint.
- Timeline/report views derived from low-level recording materials without storing high-level conclusions as tool facts.
