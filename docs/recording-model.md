# Recording Model

DbgAtlas 的 recording 能力负责采集、过滤和归档低层运行时事件材料。Recording 以 ETW API 为主线，TTD recording 也归入同一个 `recording` namespace，而不是单独暴露一套平行概念。

recording 是独立产品能力，不依赖 debug session。调试和外部分析 workflow 可以引用 recording artifact，但 recording lifecycle 本身不要求已有 debug session。

## Goals

- 使用 Windows ETW API 采集开发调试所需的低层事件。
- 以 process tree 为主过滤维度，减少无关系统噪声和 trace 体积。
- 同时保留过滤后的 ETL、recording metadata 和结构化事件 JSONL。
- 让人或 agent 可以通过 artifact id、operation id 和 recording id 引用事件材料。

## Non-goals

- 不以 WPR/WPAExport 作为主采集链路。WPAExport 只作为后续诊断、比对或 fallback 方向。
- 不定义高层 Case、Evidence 或 Timeline 结论 schema。
- 不生成全局 `timeline.jsonl`。按时间排序视图由读取方根据 category 文件中的 timestamp 合并。
- 不记录归因、根因、风险判断或其他分析结论到工具事实层。

## Lifecycle

公开 capability namespace 使用 `recording.*`。Recording lifecycle 包括：

- `recording.start`：启动一次 recording，并返回 `recording_id`、`operation_id` 和初始 artifact 引用。
- `recording.stop`：停止 recording，flush 过滤后的 ETL 和事件文件，并登记最终 artifact metadata。
- `recording.status`：查询 recording 状态、target、开始时间、已登记 artifact 和最近 operation。
- `recording.cancel`：协作式取消正在进行的 recording operation。
- `recording.kill`：当 worker 卡死或无法协作停止时终止 recording worker，并把 operation 记录为 failed 或 canceled。
- `recording.ttd`：用 `TTD.exe` 执行一次 launch / attach / monitor 录制，返回 `recording_id`、`operation_id`、状态和 artifact refs；该入口是一次性同步操作，不复用 ETW lifecycle 的 `recording.start/status/stop`。

service `0.3` 在 JSON-RPC 和 MCP 上暴露相同的 `recording.start/status/stop/cancel/kill` lifecycle；MCP input schema 与 RPC 参数一致，并继续返回相同的 recording、operation 和 artifact 引用。

CLI 方向与 service capability 对齐：

```powershell
dbgatlas recording start --project-root <path> --launch <exe> [-- <args>]
dbgatlas recording start --project-root <path> --attach <pid>
dbgatlas recording start --project-root <path> --attach <pid> --preset cpu --cpu-interval-us 1000 --cpu-max-trace-size-mb 512
dbgatlas recording status <recording-id>
dbgatlas recording stop <recording-id>
dbgatlas recording cancel <recording-id>
dbgatlas recording kill <recording-id>
dbgatlas recording ttd --project-root <path> --launch <exe> [--start-directory <dir>] --timeout-ms <ms> [-- <args>]
dbgatlas recording ttd --project-root <path> --attach <pid> --timeout-ms <ms>
dbgatlas recording ttd --project-root <path> --attach <pid> --timeout-ms <ms> --worker-identity active_interactive_user
dbgatlas recording ttd --project-root <path> --monitor <program> --timeout-ms <ms>
```

`launch` 和 `attach` 都以 process tree 为过滤核心。`attach` 不回填历史事件，只记录 `recording.start` 之后观察到的事件；CPU preset 会在启动 consumer 前用 Toolhelp 预填当时已有的子进程、线程和模块表，作为后续样本过滤和 `module+RVA` 解析上下文。

## Collection

Recording 采用 C++ ETW adapter + Rust safe wrapper 的边界：

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
- `cpu`（opt-in，不进入默认 presets）

`cpu_sampling` 只能与 `cpu` preset 同时使用。`interval_us` 默认 `1000`，范围 `125..=1_000_000`；`max_trace_size_mb` 默认 `512`，范围 `64..=4096`。CPU recording 使用 system logger 的 `SampledProfile`、process/thread/image 和 Profile StackWalk；服务内同时只允许一个活动 CPU recording。普通开发态进程权限不足时启动直接失败，不自动提权。

## Artifact Layout

Recording artifact layout 使用统一 recording namespace：

```text
artifacts/
  recordings/
    <recording_id>/
      recording.json
      trace.etl
      # CPU ETL 过滤失败时改名为 trace.unfiltered.etl
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
      traces/
        sample.run
        sample.idx
      recorder.stdout.txt
      recorder.stderr.txt
      recorder-stop.stdout.txt
      recorder-stop.stderr.txt
```

`recording.json` 记录低层可审计 metadata，包括：

- `recording_id`
- `target`：launch executable/args、attach pid，或 monitor program/cmd_line_filter。
- `mode`：`launch` 或 `attach`。
- `root_pid`
- `process_tree_filter`
- `presets`
- `started_at` / `stopped_at`
- `adapter` 和 runtime 摘要。
- `operation_id` 和主要 artifact refs。

`trace.etl` 是过滤后的 ETL artifact。CPU preset 使用有 `max_trace_size_mb` 上限的 circular ETL；过滤失败时保留为 `trace.unfiltered.etl`，并在 `recording.json.trace.target_filtered=false` 及 warnings 中明确标记，不能声称为目标专属 trace。

`events/*.jsonl` 是按 category 拆分的低层事件流。每一行是一条规范化事件，同时保留 ETW provenance 和 raw payload，方便后续审计和重新解释。

CPU 的实时 `events/cpu.jsonl` 是完整目标样本的权威数据，stop 后只在实时 consumer 失败时才从 ETL 离线回退，避免重复追加。`cpu/hotspots.json` 保存确定性排序的 top 100 frame/stack、样本占比、估算 CPU time 及进程/线程分布；`cpu/stacks.folded` 保存全部唯一 root-to-leaf 栈，可直接交给 FlameGraph 工具。`recording.stop/status` 的 `cpu_summary` 只内联 top 20；running 状态为 `null`。

TTD recording 使用同一 `artifacts/recordings/<recording_id>/` namespace。`recording.json` 的 `adapter.kind` 为 `ttd`，`traces/*.run` / `traces/*.idx` 登记为 `recording.ttd.trace` / `recording.ttd.index`，recorder stdout/stderr 登记为 `recording.recorder_output`，事件审计写入 `events.jsonl`。`recording.ttd` tool result 也会直接返回 `primary_trace_path`、`trace_paths`、`trace_index_paths` 和 `.run` 对应的 `trace_artifacts`，便于后续用 `debug.session.create` 的 `{ "kind": "file", "path": "<trace.run>" }` replay。

TTD launch target 可用 `{ "kind": "launch", "executable": <path>, "start_directory": <dir>, "args": [...] }` 指定目标启动时的 working directory；CLI 对应 `--start-directory <dir>`。该目录只影响 launch working directory，不改变 TTD trace 的 `-out` 输出目录，trace 仍由 DbgAtlas 写入当前 recording artifact 下的 `traces/`。

TTD attach target 使用 `{ "kind": "attach", "pid": <pid> }`，CLI 对应 `--attach <pid>`。安装态 service 默认在 service 进程身份下启动 `TTD.exe`；当目标进程位于当前交互用户会话且 LocalSystem 无法完成用户级 attach 时，调用方可以传 `worker_identity: "active_interactive_user"` 或 CLI `--worker-identity active_interactive_user`，让 recorder 和 timeout stop 命令都在当前交互用户身份下执行。`recording.json` 和 tool result 会记录实际请求的 `worker_identity`。

`timeout_ms` 到达时，DbgAtlas 会先执行 `TTD.exe -stop <target> -wait 10`，再等待 recorder 进程退出和落盘；只有 recorder 在 stop 后仍不退出时才兜底终止 recorder 进程，并在 metadata warnings 中记录。

若 native ETW session 启动或停止失败，`trace.etl` 可以退化为文本 fallback artifact，用于保留失败原因和审计线索。此时 `recording.json` 必须显式记录 `trace.valid_etl=false` 和 `trace.fallback_reason`，读取方不能把该文件当作可由 ETW/WPA/DbgEng 打开的有效 ETL。真实 ETL artifact 应记录 `trace.valid_etl=true`。

现有 `artifacts/profiles/` 与 `artifacts/ttd_recordings/` 是早期预留布局。当前 recording 文档以 `artifacts/recordings/<recording_id>/` 为准；旧 helper 仅作为兼容背景保留。

## Event Schema

Recording 定义低层 ETW-derived event schema，但不定义高层 Timeline/Evidence schema。每类事件应包含稳定规范字段，并保留原始 ETW 信息。

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
  "stack": {
    "frames": [
      "notepad.exe+0x1234",
      "kernel32.dll+0x1d2f0",
      "0x00007ff812345678"
    ]
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
- `cpu.jsonl`：SampledProfile 的 pid/tid、processor、IP、applied interval、地址、`module+RVA` 与 leaf-to-root StackWalk；未匹配 StackWalk 时保留 sampled IP 的 leaf-only 样本并计入质量告警。

FileIo 事件使用 best-effort completion 合并。read/write/create 等 begin-side 事件如果带有 `IrpPtr`，adapter 可暂存该事件，直到匹配的 `OpEnd` 到达后把 `nt_status`、`extra_info` 和 `completion_*` 字段合并到同一行；未匹配的 `OpEnd` 不作为独立低层事实输出，而是在 `recording.json.event_extraction` 的 counters/warnings 中说明。文件路径优先来自事件直接 path 字段；若缺失，可通过 `FileObject` / `FileKey` 路径缓存恢复，并在 `file.path_source` 中标明来源。无法恢复时保留 raw pointer 字段，不推断 path。

ETW recording 默认尽力为写入 `events/*.jsonl` 的事件补充调用堆栈。`stack.frames` 是字符串数组，输出为常见调用栈展示顺序：当前执行点/叶子帧在前，调用者在后。frame 格式优先使用 `module_name+0xoffset`；模块归属仅来自 ETW image load/unload 事件维护的进程内模块表，不读取 PDB、不调用 DIA/DbgHelp、不解析函数名。若某帧找不到模块归属，则保留原始指令地址字符串。adapter 可以同时使用 event extended data 和独立 StackWalk event，并按 timestamp、pid、thread 做 best-effort 关联；未匹配或因 bounded cache 丢弃的 StackWalk 只进入 `recording.json.event_extraction` counters/warnings。若系统、provider 或事件本身不提供 stack 数据，则省略 `stack` 字段，并在 `recording.json` 的 `stack_trace` / `event_extraction` metadata 中以 best-effort 状态和 warnings 说明降级。

`recording.json.event_extraction` 记录 adapter 对过滤后 ETL 抽取 JSONL 的质量摘要，包括写出事件数、文件数、跳过数、stack frame resolved/unresolved 计数、FileIo path resolved/unresolved 计数、OpEnd 匹配/未匹配计数、未完成 IO、IRP 复用和 dropped StackWalk。它是审计和降级说明，不替代 `events/*.jsonl` 中的事实行。

`recording.json.cpu_sampling` 记录 requested/applied/previous interval、条件恢复结果、ETL 上限、样本/StackWalk 配对计数、buffer/event loss 以及过滤和降级 warnings。DbgAtlas 仅在停止时观察到当前 interval 仍等于自身 applied 值时恢复 previous 值，避免覆盖外部 profiler 的后续修改。

字段缺失时应显式省略或使用 `null`，不能用推断值伪装为 ETW 事实。解释、假设和结论仍写入 `analysis/` Markdown。

事件行中的 `timestamp` 使用 DbgAtlas 现有 `Timestamp` JSON 形状。`artifact_id` 可以为 `null`；事件文件与 artifact id 的权威关联来自 `artifacts/artifacts.jsonl` 中登记的 metadata，而不是事件行自我声明。

## Acceptance

验收应能验证：

- launch 一个进程，并记录 root process tree 的 process/thread/image/file/registry/network 事件。
- attach 到已有 pid，只记录 start 之后的事件，并在 metadata 标明 attach mode。
- stop 后登记 `recording.json`、`trace.etl` 和 `events/*.jsonl` 的 artifact metadata。
- `workspace facts` 可返回 recording operation 和 artifact 引用。
- failed、canceled、killed recording operation 均有可审计状态；已产生 artifact 不被丢弃。
