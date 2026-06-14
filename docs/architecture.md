# DbgAtlas 架构

DbgAtlas 是面向 Windows 的调试、逆向、事件录制与问题分析平台。源码仓库产出工具本身，不承载真实分析数据；真实分析数据放在显式 analysis workspace 中。

## 分层

```mermaid
flowchart LR
  CLI["dbgatlas-cli"] --> CORE["dbgatlas-core"]
  MCP["dbgatlas-mcp (later)"] --> CORE
  CORE --> DEBUG["dbgatlas-debug"]
  CORE --> RUNTIME["dbgatlas-runtime"]
  CORE --> WS["dbgatlas-workspace"]
  CORE --> AD["dbgatlas-adapter"]
  DEBUG --> AD
  DEBUG --> RUNTIME
  DEBUG --> WS
  AD --> DBG["dbgatlas-dbgeng"]
  DBG --> SYS["dbgatlas-dbgeng-sys"]
  SYS --> DLL["native dbgeng DLL"]
```

`dbgatlas-debug` 代表 debug domain manager 边界：它定义 target、session、command 和状态模型，但 MVP 0.5 不接真实 DbgEng session。后续 DbgEng、TTD、dump eval 等能力由这个 domain manager 编排 runtime、workspace 和具体 adapter/native wrapper。

`dbgatlas-runtime` 代表运行时安装与进程策略边界：DbgEng/TTD/IDA 安装位置、symbol path、proxy、child process policy 属于 runtime config，不写入 analysis workspace manifest。

## MVP 0 边界

- `dbgatlas-model` 只放最小公共模型：`Id`、`WorkspaceRef`、`TargetRef`、`SessionRef`、`ArtifactRef`、`OperationRef`、`Timestamp`。
- `dbgatlas-workspace` 只管理磁盘事实：manifest、`artifacts/`、`analysis/`、可选 `inputs/`、artifact metadata、operation log。
- `dbgatlas-adapter` 只定义最小 adapter contract：adapter id、capability、invocation、result、error。
- `dbgatlas-core` 编排 workspace 与 adapter，不直接接触 unsafe FFI。
- `dbgatlas-cli` 是 MVP 0 的唯一入口。
- `dbgatlas-dbgeng-sys` 和 `dbgatlas-dbgeng` 只验证 native ABI hello/version，不实现 DbgEng session。

## MVP 0.5 加固边界

- `dbgatlas-debug` 定义 debug target、session state、session skeleton、command eval 请求/结果和 manager trait；它不是 DbgEng wrapper。
- `dbgatlas-runtime` 定义 runtime config、tool path、symbol path、proxy 和 process launch policy；它不拥有 workspace 数据。
- `dbgatlas-workspace` 增加受控 artifact layout helper，例如 `artifacts/sessions/<session_id>/`、`artifacts/profiles/<profile_id>/`、`artifacts/ttd_recordings/<recording_id>/`、`artifacts/reverse_sessions/<session_id>/`。
- `dbgatlas-core` 保持短调用 `invoke()`，并预留长任务 operation 状态：`running`、`success`、`failed`、`canceled`。
- 同一 debug session 的请求必须串行化；不同 session 后续可并发。状态不能依赖命令文本解析。
- `dbgatlas-adapter` 不是 session/backend 总接口；session 生命周期、worker 管理和 domain 语义由 domain manager 承担。

## 预留但不创建

第一版不创建 `dbgatlas-mcp`、`dbgatlas-ida`、`dbgatlas-etw*`、`dbgatlas-dia*`、`dbgatlas-symbol`、`dbgatlas-pe`、`dbgatlas-report`。这些能力在 core/workspace/adapter API 稳定后再引入。

IDA 路线优先走 `ida-pro-mcp` supervisor/worker 模式，由 DbgAtlas 作为入口和 artifact/operation 记录方编排；它不走 C++ native adapter 主线。

## 不做的事

- 不使用隐藏 `.dbgatlas`。
- 不建立中心化 `protocol` crate。
- 不提前设计完整 GUI。
- 不提前封装完整 DbgEng。
- 不引入复杂序列化框架。
