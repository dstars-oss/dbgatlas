# Workspace Model

DbgAtlas 的真实分析数据放在显式 analysis workspace 中。workspace 是工具事实层，不保存人或模型的高层结论为客观事实。

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
    reverse_sessions/
  analysis/
  inputs/                 # 可选
```

## 目录职责

- `artifacts/`：DbgAtlas 管理的工具产物、原始输出、metadata 和可重建索引。
- `analysis/`：人或模型写作的 Markdown 笔记、假设、结论和报告。
- `inputs/`：人工放入的初始输入材料，可选创建。

## Artifact Layout

当前 workspace 保留全局索引文件，并约束领域 artifact 目录：

- `artifacts/sessions/<session_id>/`：debug session 的 commands、events、transcript、raw output。
- `artifacts/recordings/<recording_id>/`：统一 recording namespace，承载 ETW recording 和 TTD recording 的低层输出。
- `artifacts/profiles/<profile_id>/`：早期 profiling / sampling 预留布局；后续需要时再决定迁移、兼容或保留策略。
- `artifacts/ttd_recordings/<recording_id>/`：早期 TTD recording 预留布局；后续 TTD 应优先并入统一 `recordings` namespace。
- `artifacts/reverse_sessions/<session_id>/`：IDA 或其他 reverse workflow 的低层工具输出。

workspace API 只接受位于 `artifacts/` 下的相对路径，拒绝绝对路径和 `..` 穿越。dump、trace、transcript、memory output 都按敏感 artifact 处理。

这些目录是可扩展布局；具体工具能力由 service/API 实现决定。

ETW recording artifact 目录为：

```text
artifacts/recordings/<recording_id>/
  recording.json
  trace.etl
  events/
    process.jsonl
    thread.jsonl
    image.jsonl
    file.jsonl
    registry.jsonl
    network.jsonl
```

`recording.json` 保存 target、mode、process tree filter、presets、start/stop timestamp、adapter/runtime 摘要和 operation/artifact refs。`trace.etl` 保存过滤后的 ETL。`events/*.jsonl` 保存按 category 拆分的低层 ETW-derived event records，不生成全局 `timeline.jsonl`。

## Manifest

`dbgatlas-workspace.json` 记录：

- `schema_version`
- `workspace_id`
- `created_at`
- `tool.name`
- `tool.version`

当前不定义完整 Case/Evidence/Timeline schema。后续只有在具体能力需要稳定交换格式时再补 schema。

当前仓库没有独立 `schemas/` 目录作为稳定交换格式来源；service 和 workspace 代码中的 JSON/JSONL shape 先服务已实现能力。新增 Case/Evidence/Timeline 等 schema 前，需要先有对应能力、迁移策略和验证入口，避免把尚未实现的高层解释层伪装成工具事实层。

runtime config 不属于 manifest。DbgEng/ETW/TTD/IDA 安装路径、symbol path、proxy、recording presets 和 worker process policy 由 `dbgatlas-runtime` 管理。

## Metadata

`artifacts/artifacts.jsonl` 记录 artifact id、kind、相对路径、创建时间、可选 operation id 和描述。

`artifacts/operations.jsonl` 记录 operation id、adapter id、capability、状态、摘要、产生的 artifact 引用和可选 raw output 引用。operation 状态包括 `running`、`success`、`failed`、`canceled`。

`artifacts/command_audit.jsonl` 记录面向 agent 和人工审计的命令级索引，包括 operation id、可选 session id、capability、命令文本、状态、artifact 引用和 raw output 引用。它是低层工具事实，不存放推断、归因或结论。

`analysis/` 仍只放 Markdown 解释层。高层语义、假设、结论和报告不应写入 `artifacts.jsonl` 或 `operations.jsonl` 伪装成工具事实。

## Agent Facts

AI agent 应优先通过稳定入口读取 workspace facts，而不是猜测目录结构：

```powershell
dbgatlas --json workspace facts <project_root>\dbgatlas
```

MCP 场景下使用 `workspace.facts` tool，参数为内部 workspace 路径：

```json
{
  "path": "D:\\case-001\\dbgatlas"
}
```

Codex 通过 service-hosted HTTP MCP endpoint 接入，token 从本地环境读取：

```toml
[mcp_servers.dbgatlas]
url = "http://127.0.0.1:7331/mcp"
bearer_token_env_var = "DBGATLAS_TOKEN"
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

recording workflow 返回同一类引用字段，并增加 `recording_id`。报告或 agent 不应猜测 `events/` 内部文件名来确认一次 recording 是否完成，而应优先通过 operation/artifact refs 找到 `recording.json`、`trace.etl` 和 category event artifacts。

报告或分析笔记应在 `analysis/` 下用 Markdown 引用这些 id，例如 `operation_id=op-...`、`artifact_id=artifact-...`、`session_id=session-...`。解释、假设和结论只写入 Markdown，不写入 facts JSONL。
