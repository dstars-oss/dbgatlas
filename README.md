# DbgAtlas

DbgAtlas is a Windows investigation platform for debugging, reverse engineering, event recording, and reproducible problem analysis.

This repository contains the tool implementation. Real investigation data belongs in an explicit analysis workspace, not in hidden repository state.

MVP 0 and 0.5 contain:

- A Rust workspace split into `model`, `workspace`, `adapter`, `core`, `cli`, and a minimal `dbgeng` native wrapper.
- Debug domain skeletons in `dbgatlas-debug`, without a real DbgEng session implementation yet.
- Runtime configuration types in `dbgatlas-runtime`, kept separate from workspace manifests.
- A visible analysis workspace layout with `artifacts/`, `analysis/`, and optional `inputs/`.
- Controlled artifact helpers for sessions, profiles, TTD recordings, and reverse sessions.
- A C++20 native DLL skeleton that exposes only C ABI hello/version functions.
- CLI commands for workspace initialization, workspace inspection, and native ABI version checks.

Useful commands:

```powershell
cargo metadata --format-version 1
cargo test --workspace
cargo run -p dbgatlas-cli -- workspace init .\scratch-workspace --with-inputs
cargo run -p dbgatlas-cli -- workspace info .\scratch-workspace
cargo run -p dbgatlas-cli -- native version
```
