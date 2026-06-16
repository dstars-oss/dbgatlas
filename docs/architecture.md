# DbgAtlas 架构

DbgAtlas 是面向 Windows 的调试、逆向、事件录制与问题分析平台。源码仓库产出工具本身，不承载真实分析数据；真实分析数据放在显式 analysis workspace 中。

## 分层

```mermaid
flowchart LR
  CLI["dbgatlas-cli"] --> SVC["dbgatlas service\nJSON-RPC HTTP"]
  MCP["dbgatlas-mcp"] --> SVC
  UI["future UI"] --> SVC
  SVC --> CORE["dbgatlas-core"]
  SVC --> DEBUG["dbgatlas-debug"]
  SVC --> RUNTIME["dbgatlas-runtime"]
  SVC --> WS["dbgatlas-workspace\ninternal storage"]
  SVC --> WORKER["per-session worker"]
  CORE --> AD["dbgatlas-adapter"]
  DEBUG --> AD
  DEBUG --> RUNTIME
  DEBUG --> WS
  WORKER --> DBG
  AD --> DBG["dbgatlas-dbgeng"]
  DBG --> SYS["dbgatlas-dbgeng-sys"]
  SYS --> DLL["native dbgeng DLL"]
```

`dbgatlas service` 是产品运行时控制面。CLI、MCP 和后续 UI 都是 service client，不是架构核心。对外 API 使用 JSON-RPC 2.0 over loopback HTTP，默认要求 bearer token；长操作可以通过 HTTP streaming/SSE 返回 progress/output，但取消必须显式请求，断线不等于取消。

对外 API 不暴露 `workspace.*`、`project.*` 或 `worker.*` 业务资源。调用方在创建 session 时传入 `project_root`；service 内部把它解析为 `<project_root>/dbgatlas` 作为 analysis workspace，并懒创建该目录。`workspace` 仍是内部持久化模型，负责 manifest、artifact metadata、operation log 和受控 artifact path。

`session` 是公开生命周期对象，`worker` 是内部运行时对象。MVP 默认一个 session 绑定一个 worker；创建 session 时创建 worker，关闭或 kill session 时结束对应 worker。worker 由 service 的 Windows Job Object 管理，service 退出时 worker 不应残留。

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

## MVP 1 service 边界

- 同一个 `dbgatlas.exe` 提供 `service run` 开发模式、安装态 Windows service 入口和普通 CLI client 命令。
- Windows service lifecycle 由 `dbgatlas service install/start/stop/status/uninstall` 管理。安装时复制 `dbgatlas.exe`、`dbgatlas-worker.exe` 和 `dbgatlas_dbgeng.dll` 到 `%ProgramData%\DbgAtlas\bin\`，SCM 只指向该安装目录，避免锁住开发构建产物。
- `debug.session.create` 接收 `project_root` 和 target，返回 `session_id`；后续 `debug.eval`、`debug.modules`、`debug.threads`、`debug.stack`、`debug.session.close` 和 `debug.session.kill` 只需要 `session_id`。
- 外部 service API 表达产品能力；内部 worker protocol 表达低层执行、状态、artifact 写入清单和进程控制。两者分层演进。
- Worker identity 按 capability policy 选择：debug 默认 user session，ETW/WPR 默认 LocalSystem，IDA 默认 user session。权限不足时返回结构化错误，不自动提权。

## 预留但不创建

第一版不创建 `dbgatlas-ida`、`dbgatlas-etw*`、`dbgatlas-dia*`、`dbgatlas-symbol`、`dbgatlas-pe`、`dbgatlas-report`。这些能力在 core/workspace/adapter API 稳定后再引入。

MVP 2 引入 `dbgatlas-mcp` 作为薄入口层。它通过现有 service/domain workflow 暴露 MCP tools，不复制 debug/session/recording 业务逻辑。

IDA 路线优先走 `ida-pro-mcp` supervisor/worker 模式，由 DbgAtlas 作为入口和 artifact/operation 记录方编排；它不走 C++ native adapter 主线。

## 不做的事

- 不使用隐藏 `.dbgatlas`。
- 不建立中心化 `protocol` crate。
- 不提前设计完整 GUI。
- 不提前封装完整 DbgEng。
- 不引入复杂序列化框架。
