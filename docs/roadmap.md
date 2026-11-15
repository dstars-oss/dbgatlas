# Current Implementation Baseline

[PLAN.md](../PLAN.md) owns iteration order and acceptance status. The [architecture specification](architecture.md) and [environment/session model](environment-session-model.md) describe the target design. This file records the local implementation baseline; it is not a second roadmap or a claim that planned capabilities are available.

## Implemented local paths

| Area | Current implementation | Owning source |
| --- | --- | --- |
| Entry points | One loopback listener exposes /rpc and /mcp, without authentication; browser Origin checks remain | [rpc_http.rs](../crates/dbgatlas-service/src/service/rpc_http.rs) |
| API inventory | One method registry supplies RPC routes and MCP tools | [method_registry.rs](../crates/dbgatlas-service/src/method_registry.rs) |
| Sessions and executions | Service-owned accepted tasks, finite waits, same-instance deduplication, canonical session.* / execution.* with legacy aliases | [execution.rs](../crates/dbgatlas-service/src/service/execution.rs) |
| Debug execution | Per-session engine owner, named-pipe JSONL v9 response/event dispatch and independent control pipe; epoch-checked interrupt/cancel, file/launch targets and explicit invasive/noninvasive attach | [worker protocol](../crates/dbgatlas-worker-protocol/src/lib.rs), [host_debug.rs](../crates/dbgatlas-service/src/service/host_debug.rs) |
| Command output | Raw command previews, live/final UTF-8 cursor reads and separate session streams; full files retain session/execution producer attribution | [output_read.rs](../crates/dbgatlas-service/src/service/output_read.rs) |
| ETW | Dedicated recording worker owns NativeEtwController, native session and real-time consumer; service owns admission and final artifact registration | [host_recording.rs](../crates/dbgatlas-service/src/service/host_recording.rs) |
| TTD recording | Dedicated worker runs identity helpers, recorder and stop tools; helper ownership handshake and Job protect process cleanup | [recording_support.rs](../crates/dbgatlas-service/src/service/recording_support.rs) |
| Persistence | Versioned session.json; append-only JSONL operation updates merged by ID/sequence, immutable input/context and output_error; existing artifact paths retained | [workspace implementation](../crates/dbgatlas-workspace/src/lib.rs) |
| Generic adapter | Synchronous invocation and artifact registration; not the complete debug backend lifecycle | [adapter](../crates/dbgatlas-adapter/src/lib.rs), [core](../crates/dbgatlas-core/src/lib.rs) |
| Installation | Windows service, isolated runtime payload and service.update workflow | [install configuration](install-configuration.md) |

## Implemented lifecycle boundaries

- Active sessions have one dedicated worker instance. No pools, reuse, transparent restart or migration.
- Controller, worker and target environment fields are separate. `local` is the default alias; unknown boot identity stays null and service_instance remains distinct.
- Normal close preserves targets; forced cleanup verifies local process creation evidence on a retained handle. Attached targets are not registered as owned.
- Failed cleanup remains visible and retryable. ETW recovery verifies collection name/path, and CPU reservations are retained until cleanup is confirmed.
- Public execution facts and output availability are separate. Accepted operations are journaled before execution; unknown outcomes are not replayed.
- Internal handles, pipes, locks and native contexts stay out of the public Session model. Existing DebugSessionState fields are compatibility projections, not target execution truth.

## Interaction boundaries

[3C](iteration-3c-validation.md) implements the Session + Execution contract over RPC and ordinary MCP tools. Action waits default to 1000 ms, observations to zero, with a 30000 ms maximum. Expiry and client disconnect do not cancel execution. `execution.get` can wait for finalization; close seals admission and coordinates the existing task, while explicit kill remains separate. Busy sessions reject new commands without queuing them.

`debug.eval` and helper aliases enter the same coordinator. Explicit legacy debug `timeout_ms` is rejected and points to `wait_ms`; no execution-timeout worker kill remains. Cancellation uses PASSIVE and target interruption uses ACTIVE with an observed epoch. Accepted control is distinct from the original result. `operation.stream` delegates to bounded `execution.read` instead of returning an unbounded event array.

Read cursors are resource- and service-instance-bound. Default/max text budgets are 8/64 KiB; at most 64 Artifact references accompany each response. Native output, engine-event JSONL and lifecycle-event JSONL keep separate source labels. Closing retains execution output. Same-workspace file inputs may explicitly reference an Artifact without changing its producer; external inputs and old unowned records remain unowned.

[3A](iteration-3a-validation.md) selected MCP 2025-06-18 ordinary calls plus Execution queries. The current protocol adapter still negotiates the legacy 2024-11-05 shape, returns text content, and lists compatibility tools. Version negotiation, structured results, default tool-list pruning and CLI convergence remain in 3D. Tasks, notifications and real AI turn scheduling are not established by these service tests.

Execution get/read and retry keys retain only this service instance's runtime association. `execution.list` reads persisted facts and projects old unfinished records as unknown; this does not restore a debugger after restart. Internal Operation types and JSONL names are retained for compatibility.

SQLite history, remote backend transport, stable cross-session target matching, a Linux backend and a management page remain planned. Raw command access does not by itself establish a supported kernel or remote debugging lifecycle.

## Validation evidence

- [Iteration 1 validation](iteration-1-validation.md): local release/install and representative debug/ETW smoke results, with explicit untested paths.
- [HTTP authentication removal](http-auth-removal-validation.md): local entry-point and installed-service checks.

Iteration 2 evidence and explicit untested paths are recorded in [iteration-2-validation.md](iteration-2-validation.md). Historical reports above remain unchanged.

3B validation, release payload checks and the missing installed-service/MCP prerequisite are recorded in [iteration-3b-validation.md](iteration-3b-validation.md).

3C implementation and validation evidence are recorded in [iteration-3c-validation.md](iteration-3c-validation.md); installed-service acceptance remains explicitly separate.
