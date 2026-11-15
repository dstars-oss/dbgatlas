# 迭代 3C：Session + Execution 验证记录

日期：2026-11-15。前置提交：`1430e61`（3B）。本记录对应其后的 3C 工作区实现；完整迭代 3 的 MCP/CLI 与安装态验收仍在 3D。

## 实现范围

- API/包版本 0.4.0；session.open/exec/get/list/interrupt/close/kill/read/help 与 execution.get/list/cancel/read 可从 RPC、普通 MCP tools/call 使用。内部 Operation 标识和 JSONL 保留，兼容入口不另建一份执行。
- 接受并记账后由服务线程持有执行；动作等待默认 1000 ms，观察默认 0，最大 30000 ms。等待到期或 HTTP 断线不取消、不释放会话占用。exec 同实例请求键可核对原执行；忙时拒绝，不排队。
- status 表示执行结果，finalized 表示服务任务收尾结束，output_complete 另要求没有归档错误或 unknown。执行成功而登记失败保留 success 和 output_error，避免调用者重放。
- session.engine/run_epoch、目标状态及 active_execution_id 分开。普通命令的实际线程/栈帧上下文由 worker owner 在执行开始采样；打开动作只记录当时已知的请求和 worker 信息。
- close 先封住新普通命令，协调在途执行后 detach；重复 close 返回同一执行。cancel 返回独立 control_execution 与原 execution，迟到 cancel 不覆盖原 success。批次记录父子关联，运行、取消、unknown 或关闭时停下，并给出未执行索引。
- 读取默认 8 KiB、最大 64 KiB、最小 4 字节源文本预算，另有最多 64 个 Artifact 引用。游标绑定资源和服务实例；UTF-8 尾部跨页保留，非法字节/截断产生持续可见的 gap。`.part` 转正式文件和会话关闭不重置游标。异步事件按 source 分开的 JSONL 文本片段返回，需要跨页拼接。
- 完整输出、保留的中断输出登记 Session/Execution 来源；会话异步流仅绑定 Session。文件目标可显式引用同一 workspace 的 Artifact，复用不改 producer；外部输入不虚构来源。
- 默认 attach 继续 noninvasive/no-suspend；显式 invasive 才接管运行控制。worker v9 防止旧 worker 静默忽略模式，DbgEng ABI 0.7.0 增加 attach mode 入口，旧 ABI 函数保留默认语义。

## 契约与兼容变化

`debug.eval` 等别名共享协调器。显式旧 debug `timeout_ms` 在接受前拒绝，并指引 `wait_ms`、execution.cancel 和显式 session.kill，不静默反转参数语义。startup_timeout_ms、录制时长和服务更新超时维持各自含义。`operation.stream` 改为 execution.read 的有界 items/next_cursor/has_more/complete/gap 形状；旧 events 数组不再是此入口的返回。

动作预览仅用于展示，响应中的 output.cursor=null 表示从头读，包括预览已显示的内容。execution.get 带 wait_ms 而不带 after_seq 时等任务收尾；session.get 不带 after_seq 时即时返回观察。execution.list 可读持久化历史，旧未完成记录投影为 unknown，不声称恢复后端现场。

## 验证结果

| 层次 | 已观察的结果与本地证据 |
| --- | --- |
| 工作区测试 | `cargo test --workspace`：238 项通过，包含 service 136 项；`temp/iteration3c-workspace-tests.log`。最后的查询错误隔离和观察型 attach 控制修正后，service 136 项再次通过：`temp/iteration3c-final-service-tests.log` |
| 新增契约测试 | 8 项：有限等待/去重/忙拒绝、四字节预算与 UTF-8 游标、非法 UTF-8 与持续 gap、关闭协调/迟到取消、Artifact 来源与复用、后端成功但归档失败、get 等收尾与批次父子关系、旧 deadline 拒绝及 stream 有界读取 |
| 真实 Store DbgEng | 最终 MSVC release worker + 10.0.29680.1004，14 组场景通过，含 TTD 回放与 invasive kill/crash；`temp/worker-interaction-6a51f0d1/summary.json`、各 RPC/MCP 响应、原始输出及事件；汇总 `temp/iteration3c-release-store.log` |
| release payload | 维护脚本 `script/build-release-install.ps1 -BuildOnly` 成功，生成 dbgatlas.exe、dbgatlas-worker.exe、dbgatlas_dbgeng.dll、dbgatlas_etw.dll；`temp/iteration3c-release.log` |
| release 真实运行 | SDK 10.0.28000.2957 + 最终 release 程序，13 组场景通过，含外部 invasive 目标在显式 kill/worker 崩溃后存活；`temp/worker-interaction-1eb44222/summary.json`、`temp/iteration3c-release-smoke.log` |
| 原控制通道回归 | release 上 13 组原 3B 场景通过，含自然退出、迟到取消、配合/不配合扩展、关闭竞争和 worker 死亡清理；`temp/worker-interaction-a8c513c0/summary.json`、`temp/iteration3c-control-regression.log` |
| 安装态 | 重新检查未发现 DbgAtlas SCM 服务，也没有已连接的 DbgAtlas MCP 工具。未执行 service.update/restart/health；开发态 HTTP/MCP 不作为安装态验收替代 |

真实场景覆盖：wait=0 打开、短等待后同 worker 继续、实时输出与归档连续游标、请求键去重与冲突、客户端断线后核对、after_seq 有限观察、g 后拒绝新命令、经 MCP tools/call 中断、大于 64 KiB 输出期间取消及完整读取、关闭竞争与关闭后读取、批次取消/目标运行停止、显式两种 attach 模式及外部目标在 kill/crash 后存活、异步输出来源、TTD 离线回放和真实 Dump Artifact 输入。

invasive attach 首轮暴露了没有初始停止回调时 WaitForEvent 持续等待的问题；已复用 launch 的初始事件回调并通过实际运行/中断/保留目标测试。微软文档说明 attach 需经 WaitForEvent 完成，并由停止事件决定等待返回：[AttachProcess](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dbgeng/nf-dbgeng-idebugclient-attachprocess)、[WaitForEvent](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dbgeng/nf-dbgeng-idebugcontrol-waitforevent)。普通执行阶段仍使用 INFINITE，不把 API 的 wait_ms 传给引擎。invasive attach 还在初始事件建立现场后、发布可用会话前启用 [DEBUG_PROCESS_DETACH_ON_EXIT](https://learn.microsoft.com/en-us/windows-hardware/drivers/debugger/debug-process-xxx)，异常 worker 退出时也不得把外部目标当作平台所有的进程。

设置 detach-on-exit 的首次探针在初始事件前返回 E_UNEXPECTED；移到现场建立后、会话发布前设置，并用真实 kill/crash 验证外部目标存活。

一次整体测试与真实 worker 同时占用 debug exe，Windows 拒绝链接器替换文件；待 fixture 清理后顺序重跑整体测试通过。一次探针读取错了 target_control 的字段位置，修正为 context.target_control 后重跑通过。这两次失败不记作功能通过证据。

## 复现

```powershell
cargo test --workspace
cmake -S tests/fixtures/interaction -B temp/interaction-fixture -A x64
cmake --build temp/interaction-fixture --config Release
cargo build -p dbgatlas-cli -p dbgatlas-service --bins --example interaction-host
python script/test-worker-interaction.py --execution --fixture temp/interaction-fixture/Release --runtime temp/iteration3-probe/runtime/amd64 --ttd temp/iteration3-probe/trace.run --dump C:/Users/dstars/Downloads/DbgAtlas-iteration3-20261115-200306.dmp
.\script\build-release-install.ps1 -BuildOnly
cargo build -p dbgatlas-service --release --example interaction-host
python script/test-worker-interaction.py --execution --binary target/release/dbgatlas.exe --fixture temp/interaction-fixture/Release --dump C:/Users/dstars/Downloads/DbgAtlas-iteration3-20261115-200306.dmp
```

临时 runtime、合成 trace/dump 和证据不提交；路径是本机 fixture 示例。脚本在 repo temp 下创建 workspace，使用保留且核对 creation time 的进程句柄清理自己的目标。

## 未覆盖与下一阶段

- 未完成安装态更新与实际 Codex 工具回合；不能推断客户端会自动并发调用、消费通知或恢复等待。
- 现有 MCP 适配仍使用 2024-11-05 响应形状及 text content，兼容工具仍列出；2025-06-18 协商、structuredContent、默认列表收敛和 CLI 专用 Session/Execution 命令在 3D。
- 工具描述字节数、真实 AI 调用轮数和 token 成本比较留在 3D，未由本轮测试推断。
- Tasks、重启恢复活动执行、跨 workspace Artifact、自动匹配目标历史、SQLite 和远端后端未实现。
- 本轮 ETW/TTD recording 覆盖工作区 mock/协议测试，未重做真实录制；TTD 本轮为已有合成 trace 的打开/回放/读取，不宣称覆盖所有 TTD 中断边界。
- 读取检测到 gap 后可以继续取回已保存字节，complete 维持 false。服务重启后 get/read 的运行期映射与游标失效；持久化 list 仍可读，不能当作 exactly-once 承诺。
