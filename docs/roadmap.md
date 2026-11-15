# Current Implementation Baseline

[PLAN.md](../PLAN.md) owns iteration order and acceptance status. The [architecture specification](architecture.md) and [environment/session model](environment-session-model.md) describe the target design. This file records the local implementation baseline; it is not a second roadmap or a claim that planned capabilities are available.

## Implemented local paths

| Area | Current implementation | Owning source |
| --- | --- | --- |
| Entry points | One loopback listener exposes /rpc and /mcp, without authentication; browser Origin checks remain | [rpc_http.rs](../crates/dbgatlas-service/src/service/rpc_http.rs) |
| API inventory | One method registry supplies RPC routes and MCP tools | [method_registry.rs](../crates/dbgatlas-service/src/method_registry.rs) |
| Sessions and operations | Public Session/OperationDetails facts, separate runtime registry; session.get and filtered session.list | [service.rs](../crates/dbgatlas-service/src/service.rs) |
| Debug execution | Per-session engine owner, named-pipe JSONL v8 response/event dispatch and independent control pipe; epoch-checked interrupt/cancel, file/attach/launch targets | [worker protocol](../crates/dbgatlas-worker-protocol/src/lib.rs), [host_debug.rs](../crates/dbgatlas-service/src/service/host_debug.rs) |
| Command output | Raw WinDbg commands, bounded text preview and full file output; modules/threads/stack/symbol/memory helpers | [host_worker_ops.rs](../crates/dbgatlas-service/src/service/host_worker_ops.rs) |
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

## Deferred behavior changes

The [iteration 3 proposal](execution-interaction.md) adopts Session + Execution, with session.* actions and execution.* tracking/cancellation/output reading. It also separates Artifact producer attribution from references by later sessions. These naming and model decisions are not implemented capabilities: current Operation types, operation.* endpoints and JSONL names below remain the implementation baseline.

[Iteration 3A](iteration-3a-validation.md) has validated the independent DbgEng prototype and selected MCP 2025-06-18 ordinary calls plus Execution queries. Finite WaitForEvent timeouts paused the synthetic target, so the planned owner thread must continuously wait while API waits remain service-owned. [3B](iteration-3b-validation.md) implements the owner event wait, cached observations, independent control and `session.interrupt`. Official WinDbg MCP command output failed in the comparison; Tasks and real AI turn scheduling remain unverified.

The explicit debug execution timeout in request_worker_with_optional_timeout currently kills the worker and marks the session as Error. Command cancellation now uses the control pipe and PASSIVE interrupt; it does not fall back to terminating the worker. Native interruption HRESULT can establish canceled; an extension whose outer Execute returns S_OK remains success. The legacy execution timeout is still destructive, pending 3C.

operation.get and operation.stream query in-memory operation data; operation.list reads persisted records. Neither their names nor persisted JSONL establish restartable debugger execution, durable event subscriptions or MCP Tasks support. Long-operation interaction is an iteration 3 acceptance item.

SQLite history, remote backend transport, stable cross-session target matching, a Linux backend and a management page remain planned. Raw command access does not by itself establish a supported kernel or remote debugging lifecycle.

## Validation evidence

- [Iteration 1 validation](iteration-1-validation.md): local release/install and representative debug/ETW smoke results, with explicit untested paths.
- [HTTP authentication removal](http-auth-removal-validation.md): local entry-point and installed-service checks.

Iteration 2 evidence and explicit untested paths are recorded in [iteration-2-validation.md](iteration-2-validation.md). Historical reports above remain unchanged.

3B validation, release payload checks and the missing installed-service/MCP prerequisite are recorded in [iteration-3b-validation.md](iteration-3b-validation.md).
