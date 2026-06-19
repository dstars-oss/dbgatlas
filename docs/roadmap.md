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
- 文档明确 per-session worker、runtime/workspace 分离、IDA native adapter + user worker 路线。

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

## MVP 3: ETW Recording And Timeline

- ETW API 优先的 `recording.*` lifecycle。
- launch/attach process tree recording。
- 内置 process/thread/image/file/registry/network presets。
- 过滤后 ETL、recording metadata 和按 category 拆分的事件 JSONL。
- 低层事件材料作为后续 timeline/report/reverse workflow 输入。

## MVP 4: IDA Core Functions

- 已有基础链路：IDA database target、native IDA adapter dynamic loading、active interactive user worker 中的 reverse session、explicit runtime address / module base / IDA image base -> IDA function mapping。
- 对标 `ida-pro-mcp` Core Functions：`lookup_funcs`、`int_convert`、`list_funcs`、`list_globals`、`imports`、`decompile`、`disasm`、`xrefs_to`、`xrefs_to_field`、`callees`。
- Core Functions 通过 service RPC 和 MCP tools 暴露，保持稳定 JSON 返回结构；输入和 artifact 约定见 `docs/reverse-core-functions.md`。
- reverse workflow 低层输出、operation records 和 artifact metadata 可被 `analysis/` 中的 reverse notes 引用。

## MVP 5: Report And AI Workflow

- Markdown report workflow。
- AI agent 调用 core/MCP。
- 报告引用 artifacts 和 operation records，不伪装成工具客观事实。
