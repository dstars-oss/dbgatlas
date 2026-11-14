# Workspace Model

DbgAtlas 的真实分析数据放在显式 analysis workspace 中。操作与产物是工具事实层，analysis 保存解释，不把人或模型的高层结论作为工具事实。本文件先定义目标存储职责，再列出当前 JSONL 实现；实施顺序见 [PLAN.md](../PLAN.md)。

## 目标存储职责与阶段

- 操作统一关联输入、执行上下文、状态和输出；文件通过引用关联，不再引入独立 Observation 业务对象。自发事件归入会话记录。
- 会话绑定运行中的调试上下文，历史关联目标；重新打开同一 Dump/回放可查阅此前输出，但不共享当前线程、栈帧或回放位置。历史结果不自动当作当前现场或缓存命中。
- 迭代 2 建立环境/目标/会话关联及逻辑输出与执行侧路径的边界，保留现有 JSONL；不在本轮创建 SQLite 或全量重建数据模型。
- 迭代 4 在每个 workspace 引入本地 SQLite，保存操作原始记录和输出索引；小输出可入库，大文本、Dump、TTD、ETL 等保留文件。数据库位置、表结构、版本迁移和原记录导入在该轮确定，不丢弃已有历史。
- SQLite 中的原始操作记录不是可随时重建的缓存；可重建的派生索引要单独界定。一次操作可有多个输出，同一文件可被后续会话引用，不能随一个会话关闭而删除。
- 数据库由所属服务管理，不让远端节点直接共享写入。迭代 5 才实现远端产物取回，必须区分远端已生成、本地已归档、缺失或部分结果。
- 迭代 6 完善稳定目标身份和跨会话检索。路径或 PID 相同不保证身份相同；符号版本、线程、回放位置等上下文影响历史输出是否可复用。

## 当前 JSONL 实现

## 初始结构

```text
analysis-workspace/
  dbgatlas-workspace.json
  artifacts/
    artifacts.jsonl
    operations.jsonl
    sessions/
    recordings/            # recording artifacts
    profiles/
    ttd_recordings/
  analysis/
  inputs/                 # 可选
```

## 目录职责

- `artifacts/`：DbgAtlas 管理的工具产物、原始输出和原始操作/metadata 记录；不能整体视作可重建索引。
- `analysis/`：人或模型写作的 Markdown 笔记、假设、结论和报告。
- `inputs/`：人工放入的初始输入材料，可选创建。

## Artifact Layout

当前 workspace 保留全局索引文件，并约束领域 artifact 目录：

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

## Metadata

`artifacts/artifacts.jsonl` 记录 artifact id、kind、相对路径、创建时间、可选 operation id 和描述。

`artifacts/operations.jsonl` 记录 operation id、adapter id、capability、状态、摘要、产生的 artifact 引用和可选 raw output 引用。operation 状态包括 `running`、`success`、`failed`、`canceled`。

`artifacts/command_audit.jsonl` 记录面向 agent 和人工审计的命令级索引，包括 operation id、可选 session id、capability、命令文本、状态、artifact 引用和 raw output 引用。它是低层工具事实，不存放推断、归因或结论。

`analysis/` 仍只放 Markdown 解释层。高层语义、假设、结论和报告不应写入 `artifacts.jsonl` 或 `operations.jsonl` 伪装成工具事实。

## Agent Facts

AI agent 应优先通过稳定入口读取项目事实，而不是猜测目录结构。本地 CLI 仍可直接读取显式 workspace：

```powershell
dbgatlas --json workspace facts <project_root>\dbgatlas
```

service `0.3` 的 RPC 和 MCP 使用三个产品级入口，参数统一为 project root：

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

报告或分析笔记应在 `analysis/` 下用 Markdown 引用这些 id，例如 `operation_id=op-...`、`artifact_id=artifact-...`、`session_id=session-...`。解释、假设和结论只写入 Markdown，不写入 facts JSONL。

## 迭代 2 会话与操作记录

既有 artifacts/sessions/<session_id>/ 和 artifacts/recordings/<recording_id>/ 内增加 schema_version=1 的 session.json。录制的两个 ID 字段同值。布局、旧文件和既有编号不迁移。

operations.jsonl 追加 session_id、input、execution_context、update_seq、output_error、related_operation_id。已接受操作先落盘再执行，同 ID 的原始更新均保留，list/get 按序归并为最新状态，已记录输入和执行快照不可被后续会话变化覆盖。旧行缺失字段按未知读取。执行已完成而归档失败时分别表达结果和输出可用性，不重放动作。

worker 退出等异步事实写入会话目录 session-events.jsonl，不虚构客户端业务动作。owned-processes.json、owned-helper-*.json、recovery.json 和未确认 launch 标记是本机执行侧清理证据；不把它们当作跨主机协议或稳定目标内容身份。

本服务运行期保留终态摘要；服务重启不从 session.json 恢复可执行会话。完整跨重启查询和 SQLite 仍属于迭代 4。
