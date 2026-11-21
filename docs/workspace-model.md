# Workspace Model

DbgAtlas 将调试历史集中保存在服务数据库，将产物与分析笔记保存在显式 analysis workspace 中。工具事实与人或模型的解释可通过 ID 关联；实施顺序见 [PLAN.md](../PLAN.md)。

## 中心库与项目文件（M2）

安装服务管理 `%ProgramData%\DbgAtlas\data\history.sqlite3`。开发服务默认使用用户数据目录下的 `DbgAtlas/data/history.sqlite3`，可用 `DBGATLAS_HISTORY_DB` 指定独立数据库。并行运行的开发或测试服务应各自指定数据库。SQLite 保存五类记录，完整命令文本输出、引擎事件流、Dump、ETL 和 TTD 继续存放在所属项目。备份需要同时保留中心库和项目文件。

| 表 | 内容 |
| --- | --- |
| `workspaces` | 全局 UUID、登记根目录、manifest、注册完成标记和最近活动时间。注册意图先提交，重试可继续发布 manifest。 |
| `sessions` | 调试或录制会话摘要、服务启动实例、环境和目标、最后观察及显式输入产物引用。 |
| `executions` | 已受理的动作、不可变输入和上下文、最新状态、输出引用、归档错误和该步骤的命令审计。 |
| `artifacts` | 文件 ID、类型、相对位置、已知生产会话和生产执行。后续回放的输入引用保存在使用方会话。 |
| `history_entries` | 按序保存执行更新、会话状态变化和观察到的关键事件。更新执行摘要和追加历史共用事务。 |

首次调试会在 `<project_root>/dbgatlas` 注册格式 2 的工作区，创建的 manifest 与数据库使用同一 UUID。v1.0 前按当前格式验收，旧工作区保留原件并提示使用新目录。项目目录离线后，服务仍可从中心库查询操作、历史和来源。读取正文时核对 manifest 身份、登记路径和文件边界。关闭活动会话并移动目录后，执行 `dbgatlas workspace relink <新工作区路径>` 核对 UUID 并更新登记路径。

`dbgatlas workspace list` 和 `history.query` 从服务发现项目。管理页按会话、状态、文本及创建时间分页。历史非终态在缺少现场证据时显示 `unknown`。重启后查询历史不会重建 worker、复用运行游标或重放命令。写入新的原始事实、重新打开同一文件和跨会话目标匹配分别遵循会话归属、显式引用和后续迭代的身份规则。

## 初始结构

```text
analysis-workspace/
  dbgatlas-workspace.json
  artifacts/
    sessions/
    recordings/            # recording artifacts
    profiles/
    ttd_recordings/
  analysis/
  inputs/                 # 可选
```

## 目录职责

- `artifacts/`：DbgAtlas 管理的工具产物、原始输出和事件流；不能整体视作可重建索引。
- `analysis/`：人或模型写作的 Markdown 笔记、假设、结论和报告。
- `inputs/`：人工放入的初始输入材料，可选创建。

## Artifact Layout

按会话和录制划分文件目录：

- `artifacts/sessions/<session_id>/`：debug session 的 commands、events、transcript、raw output。
- `artifacts/recordings/<recording_id>/`：统一 recording namespace，承载 ETW recording 和 TTD recording 的低层输出。
- `artifacts/profiles/<profile_id>/`：早期 profiling / sampling 预留布局；后续需要时再决定迁移、兼容或保留策略。
- `artifacts/ttd_recordings/<recording_id>/`：早期 TTD recording 预留布局；后续 TTD 应优先并入统一 `recordings` namespace。

workspace API 只接受位于 `artifacts/` 下的相对路径，拒绝绝对路径和 `..` 穿越。dump、trace、transcript、memory output 都按敏感 artifact 处理。

这些目录是可扩展布局；具体工具能力由 service/API 实现决定。

ETW recording artifact 目录为：

```text
artifacts/recordings/<recording_id>/
  recording.json
  trace.etl
  # CPU 过滤失败时为 trace.unfiltered.etl
  events/
    process.jsonl
    thread.jsonl
    image.jsonl
    file.jsonl
    registry.jsonl
    network.jsonl
    cpu.jsonl
  cpu/
    hotspots.json
    stacks.folded
```

`recording.json` 保存 target、mode、process tree filter、presets、start/stop timestamp、adapter/runtime 摘要和 operation/artifact refs。`trace.etl` 保存过滤后的 ETL；CPU 过滤失败时明确保留为 `trace.unfiltered.etl`。`events/*.jsonl` 保存按 category 拆分的低层 ETW-derived event records；CPU 额外生成 hotspot 与 folded stack 产物，不生成全局 `timeline.jsonl`。

## Manifest

`dbgatlas-workspace.json` 记录：

- `schema_version`
- `workspace_id`
- `created_at`
- `tool.name`
- `tool.version`

当前不定义完整 Case/Evidence/Timeline schema。后续只有在具体能力需要稳定交换格式时再补 schema。

当前仓库没有独立 `schemas/` 目录作为稳定交换格式来源；service 和 workspace 代码中的 JSON/JSONL shape 先服务已实现能力。新增 Case/Evidence/Timeline 等 schema 前，需要先有对应能力、迁移策略和验证入口，避免把尚未实现的高层解释层伪装成工具事实层。

runtime config 不属于 manifest。调试/录制工具发现、symbol path、proxy、recording presets 和 worker process policy 由 `dbgatlas-runtime` 管理。

## Agent Facts

AI agent 应优先通过稳定入口读取项目事实，而不是猜测目录结构。CLI 连接服务查询显式 workspace：

```powershell
dbgatlas --json workspace facts <project_root>\dbgatlas
```

RPC 和 MCP 也提供三个按项目查询的入口，参数统一为 project root：

```json
{
  "project_root": "D:\\case-001"
}
```

| 记录类别 | 当前方法 | 返回 |
| --- | --- | --- |
| `artifacts` | `artifact.list` | `{ "artifacts": [...] }` |
| `operations` | `operation.list` | `{ "operations": [...] }`，内容是持久化 operation records |
| `command_audit` | `audit.list_commands` | `{ "commands": [...] }` |

当前 service 在内部解析 `<project_root>/dbgatlas`，调用上述方法应传 project root，不传 analysis workspace 或 `artifacts/` 的内部路径。

Codex 通过 service-hosted HTTP MCP endpoint 接入，无需配置认证凭据：

```toml
[mcp_servers.dbgatlas]
url = "http://127.0.0.1:7331/mcp"
enabled = true
startup_timeout_sec = 10
tool_timeout_sec = 120
```

debug workflow 的 CLI 和 MCP 调用都返回同一组引用字段：

- `operation_id`：本次操作的稳定引用。
- `operation_status`：`success`、`failed`、`canceled` 或 `running`。
- `artifact_refs`：本次操作产生或更新的 artifact id 列表。
- `raw_output_ref`：若存在，指向原始工具输出 artifact。
- `memory_ref`：`debug.read_memory` 成功时指向 memory artifact。

debug 文本命令使用 `result` 返回最多 64 KiB
的 inline preview，并用 `result_delivery` 返回 `truncated`、完整字节数、
inline 上限和完整 artifact 引用。完整结果在 worker 回复前原子落盘；小结果
同样保留完整 artifact。

recording workflow 返回同一类引用字段，并增加 `recording_id`。报告或 agent 不应猜测 `events/` 内部文件名来确认一次 recording 是否完成，而应优先通过 operation/artifact refs 找到 `recording.json`、`trace.etl` 和 category event artifacts。

报告或分析笔记应在 `analysis/` 下用 Markdown 引用这些 id，例如 `operation_id=op-...`、`artifact_id=artifact-...`、`session_id=session-...`。解释、假设和结论只写入 Markdown，结构化调试事实由服务记录。

## 运行现场与历史

服务在派发操作前登记输入和执行上下文，后续更新保留同一执行 ID、原始输入和按序历史。归档失败单独记录在 `output_error` 中。重启后的现场控制需要新会话。打开历史记录、分页读取输出和查看来源均通过中心库及登记的文件引用完成。

项目中的 `owned-processes.json`、`owned-helper-*.json`、`recovery.json` 及未确认 launch 标记为执行侧清理证据。高频调试事件及原始输出保留在会话目录。M1 的 JSONL 验证范围见[历史验收](management-mvp-validation.md)，当前验证范围见 [M2 中心库验收](management-m2-validation.md)。
