# 迭代 3D：MCP 与 CLI 收敛验证记录

日期：2026-11-15。前置提交：`16013df`（3C）。本记录对应其后的 3D 实现；本机验证完成，安装态验收仍缺前置条件。

2026-11-16 补充：安装态更新、健康检查和代表性 MCP 冒烟已完成，见[安装态验收记录](iteration-3-installed-validation.md)。以下保留 2026-11-15 当时的验证范围与环境记录。

## 实现与兼容范围

- API/包版本保持 0.4.0，worker v9、DbgEng ABI 0.7.0 未改动。MCP 适配独立到 `service/mcp_protocol.rs`，复用现有方法注册表与执行协调器。
- 无状态 HTTP MCP 支持 2024-11-05、2025-03-26、2025-06-18。initialize 对已知版本回显，未知版本协商到 2025-06-18；请求 header 独立选择返回格式，缺失时按 2025-03-26，不支持的 header 返回 400。GET/DELETE 返回 405，通知/客户端响应返回 202 空正文，不建立 MCP session id。
- 2025-06-18 返回一次 structuredContent 与精简 text 导航摘要；旧版本和直接 Rust `handle_mcp` 入口保留完整 JSON text。动作 failed/unknown 标记 isError；查询失败执行的结果不会把查询本身标为失败。未知方法/错误参数使用对应 JSON-RPC 错误码。
- 默认 tools/list 从 38 项减到 25 项，隐藏 operation.* 与 debug.*（除 debug.read_memory）；旧方法仍可通过 RPC 和 MCP tools/call 调用。service/recording/artifact/audit 职责保留。session.help 提供按需指引。
- CLI 新增 session open/exec/get/list/interrupt/close/kill/read/help 与 execution get/read/cancel/list。命令原样发送，支持有限等待、游标、同实例请求键，不自动重试、拆分或轮询。JSON 输出保留信封；RPC 错误和动作 failed/unknown 返回非零，已接受的 running 及成功观察失败执行返回零。旧 debug 命令保留但从主帮助隐藏，read-memory 保持可见并支持 wait-ms。
- 无 id 的通知不会执行工具。取消通知不映射到业务取消，也不提前终止服务端等待；等待最多持续调用预算 30000 ms。业务取消必须调用 execution.cancel。未声明 Tasks、SSE、progress 或资源订阅；不以 JSON-RPC 请求 id 跨客户端取消另一个请求。

版本/传输依据见 [MCP transport](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports) 和 [lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)。结构化结果只附摘要是本项目的明确取舍：[tools 规范](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)建议兼容性正文复制，但实际 Codex 客户端已验证能保留 structuredContent；其他只读取 text 的新版本客户端未验证。请求级取消的未实现边界与业务取消分开，参见 [cancellation](https://modelcontextprotocol.io/specification/2025-06-18/basic/utilities/cancellation)。

## 验证结果

| 层次 | 本轮证据与结果 |
| --- | --- |
| 窄范围 | `cargo test -p dbgatlas-service -p dbgatlas-cli` 通过；CLI 原生命令/参数互斥/退出状态、MCP 协商/响应形状/通知/发现面共新增 7 项测试；`temp/iteration3d-narrow-tests.log` |
| 工作区 | `cargo test --workspace`：245 项通过、0 失败；`temp/iteration3d-workspace-tests.log`。测试临时目录重定向到 repo temp |
| MSVC release | 维护脚本 `script/build-release-install.ps1 -BuildOnly` 成功，四文件 payload 完整：dbgatlas.exe、dbgatlas-worker.exe、dbgatlas_dbgeng.dll、dbgatlas_etw.dll；`temp/iteration3d-release.log` |
| release CLI / MCP / 实际客户端 | `script/test-client-interaction.py` 四组全部通过；`temp/client-interaction-b6463524/` 与 `temp/iteration3d-client-release.log` |
| release 执行侧回归 | `script/test-worker-interaction.py --execution`：Store DbgEng 10.0.29680.1004、真实 Dump/TTD 共 14 组通过；`temp/worker-interaction-9f74452c/` 与 `temp/iteration3d-worker-release.log` |
| 安装态 | 再次检查没有 DbgAtlas SCM 服务，当前连接工具也没有 DbgAtlas MCP。未执行安装态 service.update/restart/health 或冒烟；不手工安装/复制来替代指定验收路径 |

执行侧回归覆盖 wait=0 打开、有限等待、busy、请求键去重、断线后核对、实际执行上下文、实时输出转归档连续游标、大输出期间取消、批次父子关系、关闭协调与关闭后读取、Artifact 来源、invasive/default attach、外部目标在 kill/crash 后存活、会话异步输出、TTD 回放及 Dump Artifact 输入。旧 MCP 调用形状仍可通过兼容路径完成。

实际客户端使用本机 Codex `26.1111.2228.0` 的 app-server；在 repo temp 中创建隔离配置与 ephemeral 测试上下文，通过其 `mcpServerStatus/list` 和 `mcpServer/tool/call` 调用真实服务。HTTP relay 保存初始化、版本 header、请求和响应。客户端实际协商 2025-06-18，并在后续调用携带对应 header。六次工具调用均保留 structuredContent；短命令原始输出只在结构化正文出现。

该验证没有发起模型 turn，不证明模型调度、并发工具调用、通知消费或 UI 渲染行为。CLI 的真实流程另覆盖 launch、help、短命令、有限等待、去重、get/read/list、JSON 错误非零退出、协作取消、运行目标中断、旧 debug eval、正常关闭和关闭后读取。测试只清理按 creation time 核对的自建目标。

## 开销与调用观测

比较同一 JSON 序列化口径（compact UTF-8，无 ASCII 转义）的完整 tools/list 数组：3C 证据 `temp/worker-interaction-6a51f0d1/` 为 38 项 / 28351 字节；3D 为 25 项 / 18984 字节，减少 9367 字节（约 33.0%）。这是描述字节数，不是 tokenizer token 数，也不包含客户端额外包装。

以下为一次 release 实测，包含本机 app-server 与 HTTP relay 往返，不是统计性能保证。result_bytes 统计客户端返回的工具结果 JSON，text_bytes 只统计 text 字段；短命令及长命令均使用本地合成 fixture。

| 工作流 / 调用 | 耗时 ms | text 字节 | 完整结果字节 |
| --- | ---: | ---: | ---: |
| 短命令 session.exec | 84.75 | 253 | 4630 |
| 慢扩展 session.exec，wait_ms=10 | 56.88 | 252 | 4546 |
| execution.get，等待完成 | 1503.30 | 223 | 1916 |
| execution.read，取完整输出 | 25.09 | 161 | 824 |
| session.exec g | 73.91 | 253 | 4730 |
| session.interrupt | 85.80 | 304 | 5207 |

短命令一次工具调用完成；长命令通过 exec/get/read 三次调用完成，g/interrupt 两次工具调用后由测试辅助 RPC 确认 idle。表中的中断耗时是控制调用响应时间，不是引擎物理停止延迟。工具调用六次均成功；没有足够同口径旧版时延数据，不声称性能提升或模型任务完成率提升。原始指标为 `inventory-metrics.json`、`codex-call-metrics.json`，wire 记录在同一证据目录。

## 复现与未覆盖项

```powershell
cargo test -p dbgatlas-service -p dbgatlas-cli
cargo test --workspace
cmake -S tests/fixtures/interaction -B temp/interaction-fixture -A x64
cmake --build temp/interaction-fixture --config Release
.\script\build-release-install.ps1 -BuildOnly
python script/test-client-interaction.py --binary target/release/dbgatlas.exe --fixture temp/interaction-fixture/Release --codex 'C:\Program Files\WindowsApps\OpenAI.Codex_26.1111.2228.0_x64__2p2nqsd0c76g0\app\resources\codex.exe' --baseline temp/worker-interaction-6a51f0d1
cargo build -p dbgatlas-service --release --example interaction-host
python script/test-worker-interaction.py --binary target/release/dbgatlas.exe --fixture temp/interaction-fixture/Release --runtime temp/iteration3-probe/runtime/amd64 --ttd temp/iteration3-probe/trace.run --dump C:/Users/dstars/Downloads/DbgAtlas-iteration3-20261115-200306.dmp --execution
```

Codex、runtime、trace、dump 和 baseline 路径是本机测试材料示例；scratch/证据不提交。省略 --codex 可只运行 CLI 和协议探针；省略 --baseline 不做旧描述大小比较。应先结束服务/worker 探针再重建同一 profile，避免 Windows exe 占用。

未覆盖：安装态更新/健康检查/客户端冒烟、真实模型回合、Claude 客户端、新版本只读 text 客户端、SSE/Tasks、服务重启后恢复 get/read、ETW CPU 与 TTD 重新录制的本轮真实回归。录制契约由工作区回归覆盖，既有真实录制证据仍归先前迭代，不替换成本轮实测。完整迭代的分层状态见[总记录](iteration-3-validation.md)。
