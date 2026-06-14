# Adapter Model

`dbgatlas-adapter` 定义最小公共抽象，让 core 能统一调用不同能力，同时避免提前把 DbgEng、ETW、DIA 的行为揉成一个大接口。

## 最小概念

- `AdapterId`：稳定 adapter 标识，例如 `dbgeng`、`etw`、`ida`。
- `Capability`：能力标识，例如 `native.version`、`debug.raw_command`。
- `Invocation`：一次调用，包含 operation id、adapter id、capability、参数、可选 workspace root 和 artifact 输出目录。
- `InvocationResult`：调用结果，包含状态、摘要、machine payload、created artifact 输出和 raw output 输出；core 负责把 artifact 输出登记到 workspace metadata。
- `Adapter` trait：`metadata()`、`capabilities()`、`invoke()`。

## 设计限制

- 第一版不设计 session trait。
- 第一版不设计 streaming callback。
- 第一版不设计 event bus。
- 具体 native handle、COM、callback、线程亲和性留在 capability crate/native DLL 内部。
- `dbgatlas-adapter` 不是 session/backend 总接口；debug session、worker 生命周期和状态机属于 `dbgatlas-debug` 等 domain manager。

## Core 集成

`dbgatlas-core` 负责查找 adapter、构造 invocation、执行调用，并把 operation result 追加到 workspace。core 不直接调用 unsafe FFI。
