# 迭代 3A 原型验证

日期：2026-11-15。代码基线：`d2fb7d1`。范围：独立 DbgEng 原型、实际 Codex MCP 握手、官方 WinDbg MCP 对照。本文不代表 DbgAtlas 已提供 Session + Execution 接口，也不代表 3B–3D 已验收。

## 结论与实施门槛

3A 的实现路线已收敛，可以进入 3B：保留自有 DbgEng adapter；采用所有者线程持续消费事件、独立控制路径、服务层有限等待；首版 MCP 使用 `2025-06-18` 普通工具调用及 `execution.get/read` 查询。没有客户端声明 Tasks 的证据，暂不实现或声明标准 Tasks。

必须修订的原假设：**用户态有限超时的 WaitForEvent 也不是无副作用轮询**。两版引擎实测中，1200 ms 到期返回 S_FALSE，目标心跳停止。API 的 wait_ms 不能传入 WaitForEvent。正式 worker 默认持续等待事件；用 ACTIVE 请求目标停止，用 EXIT 唤醒所有者线程进行关闭协调，退出等待后仍须核对事件和目标状态。

这次原型的 23 组自动检查通过。官方 MCP 的命令输出链路存在失败，Claude Code 不在本机 PATH，真实 AI 回合中的并发工具调度、Tasks、长请求断线行为未验证。这些缺项不作为已通过项目，也不阻止实现不依赖它们的查询路径。

## 环境和方法

| 项目 | 实测环境 |
| --- | --- |
| 编译 | Visual Studio 18 2026 / MSVC x64，C++20，Release |
| 系统 | Windows 10.0.26300 x64 |
| SDK DbgEng | 10.0.28000.2957 |
| Store WinDbg | 1.2611.4001.0；DbgEng 10.0.29680.1004 |
| TTD recorder | 1.11.618.0 |
| Codex app-server | 本机 Desktop 随附 codex-cli 0.169.0-alpha.11.1 |

探针、目标、测试扩展、原始 JSONL、协议报文及临时 runtime 均在 `temp/iteration3-probe/`，不提交第三方二进制或真实分析材料。目标是本次编译的 probe_target.exe；扩展分别模拟配合 GetInterrupt 的长命令和不配合的 1500 ms 命令。记录线程 ID、单调时钟、HRESULT、引擎状态、事件回调与输出。通过进程内共享内存心跳独立核对目标是否仍执行，不能只用 GetExecutionStatus 自证。

成功打开的 live 目标加入本次 Job；验证正常 detach 后存活，再清理测试进程。结束时核对没有 probe/probe_target 残留。初次探索因符号路径被重置为 srv* 而触发看门狗，未计入通过结果；修正为打开后设置显式本地符号目录、启用 initial break，再完整重跑。Store DLL 直接从 WindowsApps 加载被拒绝；按仓库已有 runtime 部署方式复制到 temp 后验证，不修改 WindowsApps 权限。

复现入口为 `CMakeLists.txt`、`probe.cpp`、`target.cpp`、`extension.cpp`、`run.py`、`check.py`；`checks.json` 列出 23 个通过场景。它们是本次 scratch 原型，3B 应将适用场景转为维护中的集成测试，不能依赖本机 temp 长期存在。

## DbgEng 观察

| 场景 | 观察 | 实现含义 |
| --- | --- | --- |
| 短 `.echo` | Execute 返回 S_OK，原始输出含标记 | 可一次返回短结果 |
| `g` → DebugBreak → 自然退出 | g 先返回 S_OK / GO；WaitForEvent 才送达断点；再次 g 后退出回调给出 37，WaitForEvent 返回 E_UNEXPECTED，状态 NO_DEBUGGEE | 命令完成、目标运行、目标退出分别记账；E_UNEXPECTED 需结合退出证据 |
| launch / invasive attach + ACTIVE | 控制线程 SetInterrupt 返回 S_OK；所有者线程收到 break 事件；随后可继续查询，detach 保留目标 | 支持独立控制，但接受请求不等于停止已确认 |
| noninvasive/no-suspend attach | g 返回 0x80040205；ACTIVE 本身仍可返回 S_OK，后续等待无有效运行控制 | 显式暴露观察型 attach 能力；不宣称 resume/interrupt 支持 |
| 无控制请求，WaitForEvent(1200) | S_FALSE；引擎 BREAK；100 ms 心跳增量 0，两版均复现 | 禁止以短等待轮询驱动运行目标 |
| PASSIVE + 等待事件 | 请求 S_OK；最终 S_FALSE；没有新的停止回调 | 不能用 PASSIVE 成功或等待到期认定“取消已完成” |
| EXIT + 等待事件 | E_PENDING；引擎可报 BREAK，但 100 ms 心跳仍增长（本次 7 次） | EXIT 不等价于目标中断；状态必须带证据和观察来源 |
| 原生无限循环 + PASSIVE | 约 300 ms 投递，Execute 返回 0xd000013a；后续 echo 正常 | 保留后端中断码，协作取消可行 |
| 配合的扩展 + PASSIVE | 扩展检查到 GetInterrupt 并返回 E_ABORT，但外层 Execute 仍为 S_OK | 不以外层 S_OK 或取消请求推断扩展内部终态；保存原始输出 |
| 不配合扩展 + PASSIVE | 请求很快返回 S_OK，但扩展仍完成约 1500 ms 的工作，外层 S_OK | 不承诺强制取消；不能提前释放准入或启动下一命令 |
| 取消后下一命令 | 原生循环/两类扩展结束后 GetInterrupt=S_FALSE，后续 echo 标记可见 | 本次未见残留；迟到请求/关闭竞争仍需 3B 代次仲裁测试 |
| Dump | 打开、echo、k 成功；g=0x80040205，等待=E_UNEXPECTED | 文件目标能力分支，不做 live 事件空转 |

引擎枚举值在日志中为 GO=1、BREAK=6、NO_DEBUGGEE=7；它们是 backend 观察，不能直接替代平台的目标状态和执行状态。

### TTD

用户确认许可后，普通权限录制返回 0x80070005（需要管理员权限）。随后经用户授权启动提升权限的 recorder，成功录制自建程序：recorder 退出 0，guest 退出 37，trace 25,165,824 bytes。记录在 `trace.out`，材料在 `trace.run`；没有录制用户现有进程。

- Store runtime 回放：`!tt 0`、g + WaitForEvent、g- + WaitForEvent 和 k 通过。
- `.while (1) { !tt 0; !tt 100 }` 的控制线程约 300 ms 后投递 PASSIVE，约 15 ms 后 Execute 返回 0xd000013a，后续 echo 和 k 有输出。
- 此次中断后 GetExecutionStatus 仍可能为 GO，虽然命令已返回且可查询。因此 offline 不等于引擎 idle，Execute 返回不等于回放状态已稳定；正式实现须继续消费/采样并保存回放位置与来源。
- 这证明短 trace 的回放和重复 seek 可中断，不证明任意 TTD 查询、第三方扩展或长 trace 都能及时停止。

按用户要求，另在下载目录生成 `DbgAtlas-iteration3-20261115-200306.dmp`（11,139,942 bytes），是测试程序的完整用户态 Dump。已重新打开并查询栈；它是本次唯一显式保存在 temp 外的测试产物。

## MCP 客户端与官方对照

### Codex 实际握手

使用本机随附 app-server、隔离的 temp 配置，通过 `mcpServerStatus/list` 触发真实 MCP 连接；没有创建 AI 回合或改动用户配置。`mcp_probe.py` 和 `http_probe.py` 是协议探针，不是 DbgAtlas 服务。

| 项目 | 实测 |
| --- | --- |
| 客户端声明 | codex-mcp-client 0.169.0-alpha.11.1 |
| initialize 请求版本 | stdio 和 loopback HTTP 均为 2025-06-18 |
| capabilities | elicitation.form / elicitation.url；未声明 Tasks |
| HTTP | 接受 application/json 结果；初始化后携带 MCP-Protocol-Version: 2025-06-18；Accept 同时含 text/event-stream、application/json |
| 工具发现 | 读取到 inputSchema、outputSchema；toolsError=null |
| stdio 补充观察 | 探针返回 2025-11-25 时也完成了工具发现，但不能据此推断 Tasks 支持 |

原始证据：`mcp-client-wire.jsonl`、`mcp-http-wire.jsonl`、`codex-app-wire.jsonl`。本次没有验证 Desktop 聊天中 structuredContent 的模型呈现、真实工具并行或通知触发继续执行；首版工作流不依赖这些能力。Claude Code 未在 PATH 中发现，未安装新客户端，不能写成 Claude 验收通过。

### WinDbg MCP

使用本机 DbgX.Mcp.Proxy 1.2611.4001.0，stdio initialize 协商到 2025-06-18。用户在 WinDbg 打开本次 trace 并启用 MCP 后，通过 list_sessions / connect_session 显式选中带 trace 路径的窗口。多窗口时 proxy 不会自动选择，不能把 WinDbg PID 当成 DbgAtlas 的业务 session_id。

- 随包 McpSchema.json 有 16 个工具；当前连接的 tools/list 为 15 个（含 proxy 的三个会话工具）。实际列表没有 wait_for_break / stop_debugging，故不能用静态 schema 推断可调用面。
- get_target_state 返回 TtdUser、Stopped/Running、isRunnable、canExecuteCommands 等字段；request_break 将 Running 确认为 Stopped，本次响应约 10 ms。
- show_output 对短 echo、seek 和 k 返回 `Error: Unable to get command output`；g 在 Running 时被明确拒绝。错误文本亦可能同时出现在 text 和 structuredContent，未给出可追踪的执行 ID。不能将这些调用记录为成功，也不能据输出失败安全地自动重试有副作用命令。
- get_output_history 可取回整体历史，但本次未取回 echo 标记，不能冒充单次输出正确关联。
- disconnect_session 成功，list_sessions 仍见相同窗口和 trace，会话未被关闭。未关闭用户打开的 WinDbg 窗口；当前工具列表没有正常结束调试工具，关闭语义未实测。

证据：`dbgx-proxy-wire.jsonl`、`dbgx-tools.json`、`dbgx-replay-summary.txt`、`dbgx-history-summary.txt`。官方路径说明见 [WinDbg MCP 设置](https://learn.microsoft.com/en-us/windows-hardware/drivers/debuggercmds/set-up-windbg-mcp)。本次发现不支持立即替换自有 adapter；可以借鉴独立状态查询、请求 break、proxy 连接与调试会话分离。

## 3B 的具体实施约束

1. 所有者线程创建/操作/销毁会话，运行期间持续 WaitForEvent(INFINITE)；普通命令和回调不跨线程搬迁。GetExecutionStatus 仅是带来源的引擎观测。
2. 独立 interrupt handle 仅暴露 SetInterrupt；生命周期和代次用短临界区保护，不跨 Execute/WaitForEvent 持锁。接受、完成与迟到 cancel 仲裁后，才允许下一执行。
3. ACTIVE 用于可控制 live 目标的停止；PASSIVE 用于活动引擎命令的协作取消；EXIT 仅用于关闭协调等内部唤醒。无确定终态时保持 running/unknown，不凭接受取消覆盖 success。
4. native 结果保留原始 HRESULT、等待结果、最后事件和输出可用性。取消终态要求可归因的后端中断证据；扩展外层 S_OK 不足以证明已取消。
5. noninvasive attach 保持观察能力；增加显式 invasive 配置后才声明运行控制，不能静默改变旧 attach 对目标的影响。
6. worker v8 接入持续事件和独立控制后，再推进服务持有 Execution、wait_ms 与游标读取。首版 MCP 选择已验证的 2025-06-18 fallback，Tasks 后置到真实客户端证明支持。

## 尚未验收

23 组检查是原型断言，不是产品测试。持续输出归属、input callback、控制代次竞争、长期运行/大输出背压、close/kill 竞态、ETW/录制回归、真实 AI 调用并发和客户端断线均需在 3B–3D 验证。未运行产品 release 构建或安装态更新；本机未发现 DbgAtlas SCM 服务，当前连接工具列表也无 DbgAtlas MCP，不能用独立探针冒充安装验收。
