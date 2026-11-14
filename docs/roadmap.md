# Current Implementation Baseline

[PLAN.md](../PLAN.md) owns iteration order and acceptance status. The [architecture specification](architecture.md) and [environment/session model](environment-session-model.md) describe the target design. This file records the local implementation baseline; it is not a second roadmap or a claim that planned capabilities are available.

## Implemented local paths

| Area | Current implementation | Owning source |
| --- | --- | --- |
| Entry points | One loopback listener exposes /rpc and /mcp, without authentication; browser Origin checks remain | [rpc_http.rs](../crates/dbgatlas-service/src/service/rpc_http.rs) |
| API inventory | One method registry supplies RPC routes and MCP tools | [method_registry.rs](../crates/dbgatlas-service/src/method_registry.rs) |
| Debug sessions | ServiceHost manages sessions, local paths, request locks and WorkerHandle values | [service.rs](../crates/dbgatlas-service/src/service.rs) |
| Debug execution | Per-session local worker, named-pipe JSONL v6, DbgEng native wrapper; file/attach/launch targets | [worker protocol](../crates/dbgatlas-worker-protocol/src/lib.rs), [host_debug.rs](../crates/dbgatlas-service/src/service/host_debug.rs) |
| Command output | Raw WinDbg commands, bounded text preview and full file output; modules/threads/stack/symbol/memory helpers | [host_worker_ops.rs](../crates/dbgatlas-service/src/service/host_worker_ops.rs) |
| ETW | Service-hosted NativeEtwController and native session, process-tree recording and optional CPU sampling; no separate ETW worker | [host_recording.rs](../crates/dbgatlas-service/src/service/host_recording.rs) |
| TTD recording | TtdRecorderRunner starts an external recorder, with a helper for interactive-user execution | [recording_support.rs](../crates/dbgatlas-service/src/service/recording_support.rs) |
| Persistence | Visible workspace, JSONL operation/artifact/audit records and output files | [workspace implementation](../crates/dbgatlas-workspace/src/lib.rs) |
| Generic adapter | Synchronous invocation and artifact registration; not the complete debug backend lifecycle | [adapter](../crates/dbgatlas-adapter/src/lib.rs), [core](../crates/dbgatlas-core/src/lib.rs) |
| Installation | Windows service, isolated runtime payload and service.update workflow | [install configuration](install-configuration.md) |

## Gaps that affect iteration 2

- There is no platform environment model separating controller, backend host and target location. Local paths and Windows identities still flow through service/worker structures.
- A managed session directly contains a local worker binding. This is not yet the target session/backend/ownership model.
- Worker lifetime follows local service supervision. It does not establish a general policy for external or remote debugger processes.
- ETW and TTD do not use the same process path as debug sessions. A diagram placing all of them behind one existing worker would be incorrect.
- Public operation state and execution state need to be interpreted separately; the current DebugSessionState is not the complete target state model.

## Deferred behavior changes

The explicit debug execution timeout in request_worker_with_optional_timeout currently kills the worker and marks the session as Error. Cancellation can notify the worker or fall back to terminating it. These are existing behaviors, not guarantees of safe wait cancellation or independent target interrupt.

operation.get and operation.stream query in-memory operation data; operation.list reads persisted records. Neither their names nor persisted JSONL establish restartable debugger execution, durable event subscriptions or MCP Tasks support. Long-operation interaction is an iteration 3 acceptance item.

SQLite history, remote backend transport, stable cross-session target matching, a Linux backend and a management page remain planned. Raw command access does not by itself establish a supported kernel or remote debugging lifecycle.

## Validation evidence

- [Iteration 1 validation](iteration-1-validation.md): local release/install and representative debug/ETW smoke results, with explicit untested paths.
- [HTTP authentication removal](http-auth-removal-validation.md): local entry-point and installed-service checks.

This documentation refresh uses source inspection. It does not rerun those historical checks or mark iteration 2 as implemented.
