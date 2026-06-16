# Recording Model

DbgAtlas 的 recording 能力负责采集、过滤和归档低层运行时事件材料。MVP 3 先以 ETW API 为主线，后续 TTD recording 也归入同一个 `recording` namespace，而不是单独暴露一套平行概念。

recording 是独立产品能力，不依赖 debug session。debug、reverse 和 report workflow 可以引用 recording artifact，但 recording lifecycle 本身不要求已有 debug session。

## Goals

- 使用 Windows ETW API 采集开发调试所需的低层事件。
- 以 process tree 为主过滤维度，减少无关系统噪声和 trace 体积。
- 同时保留过滤后的 ETL、recording metadata 和结构化事件 JSONL。
- 让人或 agent 可以通过 artifact id、operation id 和 recording id 引用事件材料。

## Non-goals

- MVP 3 不以 WPR/WPAExport 作为主采集链路。WPAExport 只作为后续诊断、比对或 fallback 方向。
- MVP 3 不定义高层 Case、Evidence 或 Timeline 结论 schema。
- MVP 3 不生成全局 `timeline.jsonl`。按时间排序视图由读取方根据 category 文件中的 timestamp 合并。
- MVP 3 不记录归因、根因、风险判断或其他分析结论到工具事实层。

## Lifecycle

公开 capability namespace 使用 `recording.*`。MVP 3 文档目标至少包括：

- `recording.start`：启动一次 recording，并返回 `recording_id`、`operation_id` 和初始 artifact 引用。
- `recording.stop`：停止 recording，flush 过滤后的 ETL 和事件文件，并登记最终 artifact metadata。
- `recording.status`：查询 recording 状态、target、开始时间、已登记 artifact 和最近 operation。
- `recording.cancel`：协作式取消正在进行的 recording operation。
- `recording.kill`：当 worker 卡死或无法协作停止时终止 recording worker，并把 operation 记录为 failed 或 canceled。

CLI 方向与 service capability 对齐：

```powershell
dbgatlas recording start --project-root <path> --launch <exe> [-- <args>]
dbgatlas recording start --project-root <path> --attach <pid>
dbgatlas recording status <recording-id>
dbgatlas recording stop <recording-id>
dbgatlas recording cancel <recording-id>
dbgatlas recording kill <recording-id>
```

`launch` 和 `attach` 都以 process tree 为过滤核心。`attach` 不回填历史状态，只记录 `recording.start` 之后观察到的事件，并在 `recording.json` 中标明 attach mode、root pid 和 start timestamp。

## Collection

MVP 3 采用 C++ ETW adapter + Rust safe wrapper 的边界：

- C++ adapter 负责 ETW session、provider enable、实时事件消费、基础预处理、过滤和输出 flush。
- Rust wrapper 和 domain manager 负责参数校验、worker 编排、operation 状态、artifact 登记和 service/CLI/MCP 入口。
- service 仍是产品控制面；worker 是内部隔离边界，不作为外部 API 暴露。

首版 provider 配置使用内置 presets，而不是要求用户手写 provider GUID、keyword 和 level。内置 presets 至少覆盖：

- `process`
- `thread`
- `image`
- `file`
- `registry`
- `network`

后续可以在不破坏 preset 主路径的前提下增加高级 override，但 MVP 3 文档和验收不依赖用户自定义 provider。

## Artifact Layout

MVP 3 的目标布局使用统一 recording namespace：

```text
artifacts/
  recordings/
    <recording_id>/
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

`recording.json` 记录低层可审计 metadata，包括：

- `recording_id`
- `target`：launch executable/args 或 attach pid。
- `mode`：`launch` 或 `attach`。
- `root_pid`
- `process_tree_filter`
- `presets`
- `started_at` / `stopped_at`
- `adapter` 和 runtime 摘要。
- `operation_id` 和主要 artifact refs。

`trace.etl` 是过滤后的 ETL artifact。它不是完整 system-wide 原始 ETL，而是由 recording worker 根据 process tree 和 preset 过滤后的可复现材料。

`events/*.jsonl` 是按 category 拆分的低层事件流。每一行是一条规范化事件，同时保留 ETW provenance 和 raw payload，方便后续审计和重新解释。

现有 `artifacts/profiles/` 与 `artifacts/ttd_recordings/` 是早期预留布局。MVP 3 文档目标以 `artifacts/recordings/<recording_id>/` 为准；实现阶段需要决定旧 helper 的迁移、兼容或保留策略。

## Event Schema

MVP 3 定义完整的低层 ETW-derived event schema，但不定义高层 Timeline/Evidence schema。每类事件应包含稳定规范字段，并保留原始 ETW 信息。

规范字段至少包括 timestamp、category、event_type、pid、tid、process identity、image identity、operation ref 和 artifact ref。category-specific 字段只记录 ETW 或 adapter 能机械提取的低层事实。

所有 category 事件共享 envelope：

```json
{
  "schema_version": 1,
  "recording_id": "recording-...",
  "timestamp": { "unix_millis": 1781548800000 },
  "category": "process",
  "event_type": "start",
  "pid": 1234,
  "tid": 5678,
  "process": {
    "pid": 1234,
    "parent_pid": 1000,
    "image_path": "C:\\\\Windows\\\\System32\\\\notepad.exe",
    "command_line": "notepad.exe"
  },
  "operation_id": "op-...",
  "artifact_id": "artifact-...",
  "etw": {
    "provider": "...",
    "event_id": 1,
    "version": 0,
    "opcode": "start",
    "keywords": ["..."],
    "raw": {}
  }
}
```

Category-specific normalized fields:

- `process.jsonl`：process start/stop、pid、parent pid、image path、command line、exit code。
- `thread.jsonl`：thread start/stop、pid、tid、start address、exit status。
- `image.jsonl`：module/image load/unload、base address、size、image path、checksum、timestamp。
- `file.jsonl`：file create/open/read/write/close/delete/rename、path、operation、status、byte count。
- `registry.jsonl`：key/value create/open/query/set/delete、key path、value name、value type、status。
- `network.jsonl`：connect/accept/send/receive/close、protocol、local/remote endpoint、status、byte count。

字段缺失时应显式省略或使用 `null`，不能用推断值伪装为 ETW 事实。解释、假设和结论仍写入 `analysis/` Markdown。

事件行中的 `timestamp` 使用 DbgAtlas 现有 `Timestamp` JSON 形状。`artifact_id` 可以为 `null`；事件文件与 artifact id 的权威关联来自 `artifacts/artifacts.jsonl` 中登记的 metadata，而不是事件行自我声明。

## Acceptance

MVP 3 实现完成后应能验证：

- launch 一个进程，并记录 root process tree 的 process/thread/image/file/registry/network 事件。
- attach 到已有 pid，只记录 start 之后的事件，并在 metadata 标明 attach mode。
- stop 后登记 `recording.json`、`trace.etl` 和 `events/*.jsonl` 的 artifact metadata。
- `workspace facts` 可返回 recording operation 和 artifact 引用。
- failed、canceled、killed recording operation 均有可审计状态；已产生 artifact 不被丢弃。
