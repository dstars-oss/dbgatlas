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

MVP 0.5 保留全局索引文件，并开始约束领域 artifact 目录：

- `artifacts/sessions/<session_id>/`：debug session 的 commands、events、transcript、raw output。
- `artifacts/profiles/<profile_id>/`：profiling 或采样输出。
- `artifacts/ttd_recordings/<recording_id>/`：TTD recording 和相关 metadata。
- `artifacts/reverse_sessions/<session_id>/`：IDA 或其他 reverse workflow 的低层工具输出。

workspace API 只接受位于 `artifacts/` 下的相对路径，拒绝绝对路径和 `..` 穿越。dump、trace、transcript、memory output 都按敏感 artifact 处理。

这些目录是可扩展布局，不代表 MVP 0.5 已实现对应工具能力。

## Manifest

`dbgatlas-workspace.json` 记录：

- `schema_version`
- `workspace_id`
- `created_at`
- `tool.name`
- `tool.version`

MVP 0/0.5 不定义完整 Case/Evidence/Timeline schema。后续只有在具体能力需要稳定交换格式时再补 schema。

runtime config 不属于 manifest。DbgEng/TTD/IDA 安装路径、symbol path、proxy 和 worker process policy 由 `dbgatlas-runtime` 管理。

## Metadata

`artifacts/artifacts.jsonl` 记录 artifact id、kind、相对路径、创建时间、可选 operation id 和描述。

`artifacts/operations.jsonl` 记录 operation id、adapter id、capability、状态、摘要和产生的 artifact 引用。operation 状态包括 `running`、`success`、`failed`、`canceled`。

`analysis/` 仍只放 Markdown 解释层。高层语义、假设、结论和报告不应写入 `artifacts.jsonl` 或 `operations.jsonl` 伪装成工具事实。
