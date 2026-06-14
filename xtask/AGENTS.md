# AGENTS.md

`xtask` is for build, check, package, schema validation, and release automation.

- Do not put business logic or adapter behavior in `xtask`.
- Prefer narrow commands that compose existing Cargo/CMake workflows.
- Keep automation deterministic and suitable for CI.
