# GOALS

本文是 DbgAtlas 的 milestone task list。MVP 0 和 MVP 0.5 已完成工程骨架与架构加固；本文件从 MVP 1 记录后续可执行任务。

维护规则：

- 完成一个 feature 后，在同一个变更中更新对应 checkbox。
- 只有实现、文档和验证都完成后才标记为 `[x]`。
- 如果任务范围变化，先调整任务描述，再实现。
- 不把 speculative idea 勾成已完成；不为尚未进入阶段的能力提前创建空 crate。

## Completed Foundations

- [x] MVP 0: Cargo workspace、workspace init/info、artifact metadata、operation log、adapter contract、native hello/version、CLI bootstrap。
- [x] MVP 0.5: `dbgatlas-debug` skeleton、`dbgatlas-runtime` config、domain artifact layout、artifact path containment、long operation status、worker/runtime 文档。

## MVP 1: Debug Session And Minimal DbgEng Loop

目标：先建立 DbgAtlas 主服务、session lifecycle、worker supervisor 和 RPC 边界，再建立最小 debug session 闭环，让 DbgAtlas 能以受控 worker 方式打开 dump 或 attach process，执行原始 WinDbg command，并把 transcript、events、raw output 和 operation 记录进内部 project workspace。

Tasks:

- [x] 明确主服务 / session / worker 架构：外部不暴露 `workspace.*`、`project.*`、`worker.*` 业务 API。
- [x] 新增 `dbgatlas-service` dev-mode skeleton，支持 JSON-RPC HTTP `service.health` / `service.info`。
- [x] 新增内部 `dbgatlas-worker-protocol` JSONL message skeleton。
- [x] 实现 `project_root` -> `<project_root>/dbgatlas` 的内部懒创建规则。
- [x] 实现 session create / close / kill skeleton，并与 mock worker 1:1 绑定。
- [x] 实现 Windows service install/start/stop/status/uninstall。
- [x] 实现真实 named pipe worker transport。
- [x] 实现 worker 子进程启动、Job Object 绑定和退出清理。
- [x] 实现 per-session worker skeleton。
- [x] 实现 session cancel 生命周期。
- [x] 保证同一 session 请求串行化，不同 session 可并发。
- [x] 将 DbgEng bootstrap ABI 迁移为 adapter-specific `dbgatlas_dbgeng.h`。
- [x] 在 `dbgatlas-dbgeng-sys` 中绑定新的 DbgEng C ABI。
- [x] 在 `dbgatlas-dbgeng` 中提供 safe session wrapper。
- [x] 支持 open dump。
- [x] 支持 attach process。
- [x] 支持 execute raw WinDbg command。
- [x] 捕获 command output。
- [x] 支持 per-session add_symbols。
- [x] 将 transcript / events / raw output 写入 `artifacts/sessions/<session_id>/`。
- [x] 登记 debug session 相关 artifact metadata 和 operation records。
- [x] 支持 list modules。
- [x] 支持 list threads。
- [x] 支持 get stack。
- [x] 支持 read memory to artifact file。
- [x] 为 worker 卡死、cancel、kill、native failure 增加测试。
- [x] 增加 CLI debug session 命令。

Non-goals:

- 不做完整 DbgEng wrapper。
- 不做完整 GUI。
- 不把 WinDbg prompt 或 command text 当作 session 状态来源。
- 不把分析结论写进 `artifacts.jsonl` 伪装成工具事实。

Acceptance:

- [x] CLI 能创建 session、打开 dump、执行一条 raw command、关闭 session。
- [x] worker 卡死时主进程可 cancel/kill，并写入 failed 或 canceled operation。
- [x] 内部 `<project_root>/dbgatlas` 里可审计 replay：operation log、artifact metadata、transcript/raw output 均可定位。
- [x] `cargo test --workspace` 和 DbgEng 相关集成测试通过。

## MVP 2: Recording And Agent Entry

目标：完善可复现记录层，并提供 AI agent 可用的稳定入口。

Tasks:

- [x] 完善 artifact registry。
- [x] 完善 command audit log。
- [x] 稳定 CLI JSON 输出中的 operation status、artifact ref、raw output ref。
- [x] 让 `--json` 输出覆盖主要 CLI 命令。
- [x] 新增 `dbgatlas-mcp` 入口 crate。
- [x] MCP server 只调用 core / domain manager，不复制 debug/recording 业务逻辑。
- [x] 为 CLI 与 MCP 共享 workflow 增加测试。
- [x] 文档说明 AI agent 如何读取 workspace facts 并引用 artifact / operation id。

Non-goals:

- 不把 MCP 做成架构核心。
- 不提前定义完整 Case/Evidence/Timeline schema。

Acceptance:

- [x] 同一 debug workflow 可通过 CLI 和 MCP 调用 core 完成。
- [x] AI agent 可以读取 workspace facts，引用 artifact 和 operation id 写 Markdown 报告。
- [x] 工具事实层与 Markdown 解释层边界清晰。

## MVP 3: ETW Recording And Timeline

目标：以 ETW API 为主线实现独立 recording 能力，按 process tree 实时过滤和预处理低层事件，形成可复现的过滤后 ETL、recording metadata 和按 category 拆分的事件 JSONL，为后续 timeline/report/reverse workflow 提供事实材料。

Tasks:

- [ ] 明确 `recording.*` lifecycle：start / stop / status / cancel / kill。
- [ ] 设计 ETW runtime config、launch policy 和 recording worker 边界。
- [ ] 实现 C++ ETW adapter + Rust wrapper 的最小采集链路。
- [ ] 支持 launch process 和 attach pid 两类 recording target。
- [ ] 以 process tree 为主过滤维度，使用内置 process / thread / image / file / registry / network presets。
- [ ] 实时消费 ETW event，预处理、过滤并合并为低层 event records。
- [ ] 将 recording 输出写入 `artifacts/recordings/<recording_id>/`。
- [ ] 生成 `recording.json`、过滤后的 `trace.etl` 和按 category 拆分的 `events/*.jsonl`。
- [ ] 登记 recording 相关 artifact metadata 和 operation records。
- [ ] 增加 launch、attach、stop、cancel、kill 和 artifact 登记测试。

Non-goals:

- 不以 WPR/WPAExport 作为 MVP 3 主采集链路；WPAExport 只作为后续诊断、比对或 fallback 方向。
- 不生成全局 `timeline.jsonl`；按时间排序视图由读取方按 timestamp 合并 category 文件。
- 不在此阶段承诺完整高层 Timeline/Evidence schema。
- 不把推断、归因或根因结论写进工具事实 JSONL。

Acceptance:

- [ ] CLI 能以 launch 或 attach 启动一次受控 recording，并能 stop/status/cancel。
- [ ] 停止后可定位 `recording.json`、过滤后 `trace.etl` 和 category event JSONL。
- [ ] 可从 artifact 机械读取 process / thread / image / file / registry / network event records。
- [ ] failed / canceled / killed recording operation 有可审计状态，已产生 artifact 不被丢弃。
- [ ] 事件材料可被 Markdown 报告引用。

## MVP 4: IDA Bridge

目标：在 recording 事实层和可复现 artifact 机制更稳定后，把 debug session 中的 module、symbol、stack frame 与 IDA database 的函数、地址、注释工作流连接起来。

Tasks:

- [ ] 定义 IDA database target。
- [ ] 明确 `ida-pro-mcp` supervisor/worker 集成方式。
- [ ] 实现 stack frame -> module/symbol -> IDA function mapping。
- [ ] 实现 IDA navigation API。
- [ ] 实现 IDA comment API。
- [ ] 将 reverse workflow 低层输出写入 `artifacts/reverse_sessions/<session_id>/`。
- [ ] 登记 IDA 操作的 operation record 和 artifact metadata。
- [ ] 增加 debug stack frame 到 IDA function 的端到端测试。

Non-goals:

- 不把 IDA 做成 C++ native adapter 主线。
- 不把 IDA 数据库内容大块塞入 FFI。
- 不在 DbgAtlas 内部重建完整反编译器模型。

Acceptance:

- [ ] 从 debug stack frame 可跳转或定位到 IDA function。
- [ ] IDA 操作有 operation record 和 artifact metadata。
- [ ] 人/模型可基于 workspace artifact 在 `analysis/` 写 reverse notes。

## MVP 5: Report And AI Workflow

目标：建立报告工作流，让人和模型能基于工具事实层产出清晰、可引用、可审计的 Markdown 分析成果。

Tasks:

- [ ] 定义 Markdown report workflow。
- [ ] 约定 analysis notes / hypotheses / findings / final report 的目录与命名。
- [ ] 提供 AI agent 调用 core/MCP 的说明和示例。
- [ ] 支持报告引用 artifact id、operation id、session id。
- [ ] 可选生成摘要索引，但仍不把高层结论伪装成工具事实。
- [ ] 增加最小可复现 report example。

Non-goals:

- 不做复杂文档数据库。
- 不把 report schema 绑定到某个单一 LLM。
- 不要求 GUI 才能完成报告。

Acceptance:

- [ ] 从 debug/recording/reverse artifacts 可产出 Markdown report。
- [ ] 报告中的关键引用可回到 workspace facts。
- [ ] 事实、假设、结论三者在文本中可区分。

## MVP N: Platform Expansion

目标：在前述边界稳定后，按真实需求扩展能力，而不是提前制造空 crate 或大而全 schema。

Possible tracks:

- [ ] TTD replay 和 trace-to-debug workflow。
- [ ] Symbol / PDB / DIA / DbgHelp 能力深化。
- [ ] PE inspection 和 memory artifact analysis。
- [ ] dump triage automation。
- [ ] kernel-mode / driver / hypervisor 方向探索。
- [ ] package / installer / service runtime。
- [ ] 后续 UI，但必须继续调用 core / domain manager，不绕过平台核心。

Persistent constraints:

- 新 native 能力使用独立 header、DLL adapter 和 `*-sys` crate。
- `dbgatlas-adapter` 保持薄抽象，不变成万能 backend。
- runtime config 不写入 workspace manifest。
- workspace 不使用隐藏 `.dbgatlas`。
- 高层语义只进入 Markdown 解释层，除非后续明确设计了对应 schema 和迁移策略。
