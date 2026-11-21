# AGENTS.md

Rust crates use the workspace stable toolchain and the edition configured in the root `Cargo.toml`.

- Keep `dbgatlas-model` small and stable; do not add high-level analysis conclusions or full Timeline/Evidence schemas there prematurely.
- `dbgatlas-workspace` owns workspace identity, controlled artifact paths, and the SQLite catalog used by the service for workspace, session, execution, artifact, and history records.
- `dbgatlas-adapter` defines minimal adapter contracts only; concrete DbgEng/ETW/DIA behavior belongs in capability crates.
- `dbgatlas-debug` owns debug domain models and session skeletons; do not wire real DbgEng or worker process logic into it prematurely.
- `dbgatlas-runtime` owns machine/runtime configuration types; do not store runtime install paths or proxy settings in workspace manifests.
- `dbgatlas-core` orchestrates workspace and adapters, and must not directly contain unsafe FFI calls.
- `*-sys` crates are the only Rust layer allowed to expose raw native ABI bindings.

- IDA static analysis belongs to external tools; do not reintroduce IDA bindings, wrappers, or reverse workers. Do not maintain IDA-specific compatibility layers.
