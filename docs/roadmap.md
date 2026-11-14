# Roadmap

[GOALS.md](../GOALS.md) is the authoritative iteration plan. This document describes the current local implementation; remote backends, SQLite history, Linux, and management UI are future iterations.

## Current Baseline

- Cargo workspace and native CMake project.
- Visible analysis workspace initialization, inspection, artifact metadata, operation log, and command audit log.
- Minimal adapter contract with Rust core orchestration.
- Runtime config separated from workspace manifests.
- Domain artifact layout helpers and artifact path validation.
- Native C ABI boundaries for DbgEng and ETW adapters with matching Rust `*-sys` crates and safe wrappers.
- Installed Windows service lifecycle, development service mode, JSON-RPC `/rpc`, and HTTP MCP `/mcp`.
- A shared service method registry that keeps JSON-RPC routes and MCP tool schemas aligned.
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
- Low-level recording materials available as inputs for external analysis and future timeline views.

## Product boundary and next iterations

IDA static analysis uses external tools.
Debugger-native symbols, types, disassembly, and address information remain in scope.

The next iteration separates session management from backend execution and validates
the current local debugging chain across that boundary. Later iterations follow
GOALS.md; those capabilities are not part of the current implementation.
