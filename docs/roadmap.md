# Roadmap

## MVP 0: Engineering Skeleton

- Cargo workspace。
- analysis workspace 初始化和信息读取。
- artifact metadata 与 operation log。
- 最小 adapter contract。
- native CMake project。
- C ABI hello/version。
- Rust `dbgatlas-dbgeng-sys` raw binding。
- Rust `dbgatlas-dbgeng` safe wrapper。
- CLI 调用 workspace 与 native version。

## MVP 0.5: Architecture Hardening

- `dbgatlas-debug` 定义 debug target/session/command skeleton。
- `dbgatlas-runtime` 定义 runtime config、tool path、proxy 和 process policy。
- workspace 增加 domain artifact layout helper 和 artifact path 校验。
- core 增加长任务 operation 状态预留。
- 文档明确 per-session worker、runtime/workspace 分离、IDA supervisor/worker 路线。

## MVP 1: Debug Session And Minimal DbgEng Loop

- per-session worker skeleton。
- debug session create/close/cancel/kill。
- open dump。
- attach process。
- execute raw WinDbg command。
- capture command output。
- record transcript/events/artifacts。
- list modules。
- list threads。
- get stack。
- read memory to artifact file。

## MVP 2: Recording And Agent Entry

- artifact registry 完善。
- command audit log 完善。
- `--json` 输出覆盖主要 CLI 命令。
- MCP server 作为 core 的入口层。

## MVP 3: IDA Bridge

- IDA database target。
- stack frame -> module/symbol -> IDA function mapping。
- IDA navigation/comment API。

## MVP 4: ETW/WPR Timeline

- ETW/WPR recording。
- process/thread/image/file/registry/network event 提取。
- 与 module/symbol/function 关联。

## MVP 5: Report And AI Workflow

- Markdown report workflow。
- AI agent 调用 core/MCP。
- 报告引用 artifacts 和 operation records，不伪装成工具客观事实。
