# 迭代 3B：独立控制与持续状态验证

日期：2026-11-15。基础提交：`d2fb7d1`，本记录对应其上的本地 3B 改动。3A 机制依据见[原型记录](iteration-3a-validation.md)，完整接口目标见[执行交互方案](execution-interaction.md)。

## 本批实现

- DbgEng ABI 0.6：所有者线程保有 Session/COM 对象；专用 InterruptHandle 可跨线程投递控制，内部短锁核对 activity epoch。完成/关闭使旧代次失效，不将会话指针作为通用跨线程句柄。
- 永久 event/output/input callback；目标运行时所有者使用 WaitForEvent(INFINITE)。ACTIVE 请求目标中断，PASSIVE 请求命令协作取消，EXIT 只用于关闭时退出等待。
- worker 协议 v8：业务 response/event 分流、连续 snapshot 序号、独立控制 pipe。服务常驻接收器按 request_id 分发结果，迟到响应不会成为下一请求的结果；旧 snapshot 不覆盖新观察。
- 新增 RPC/MCP `session.interrupt(session_id, expected_epoch?)`；返回 accepted、epoch、native_hresult 和控制 execution_id。accepted 只表示投递，目标是否停止由后续事件核对。3B 的执行记录仍通过兼容 operation.* 查询。
- 旧 operation.cancel 使用独立 PASSIVE 通道，不再因业务锁忙自动杀 worker。原生 HRESULT `0xD000013A` 结算 canceled；扩展的外层 Execute 若返回 S_OK，保留 success。
- 异步原始文本写 `session-output.log`；有界 callback 队列记录 sequence/gap，持久化 `engine-events.jsonl`；命令原始 HRESULT 写 `engine-results.jsonl`。命令取消/失败后的 `.part` 保留，供恢复核对；尚未提供这些文件的公共游标读取和完整 Artifact 登记。
- 默认 attach 仍为 noninvasive/no-suspend，报告 target_control=false、target_state=unknown，避免把引擎 BREAK 描述为目标停止。显式 invasive 参数随 3C 的 session.open 收敛。

## 验证结果

| 验证 | 结果 / 证据 |
| --- | --- |
| Rust 工作区 | `cargo test --workspace`：229 passed，0 failed；`temp/iteration3-workspace-tests.log` |
| native 代次/生命周期 | Dump 实测覆盖重复 begin 拒绝、过期 epoch、错误中断类型、下一命令不受影响、关闭与跨线程控制/事件读取竞争；包含在 DbgEng 的 14 项测试中 |
| 管道分发回归 | 真正的 Windows overlapped pipe：读等待期间可写后续请求、超时后的迟到结果隔离、snapshot 序号防回退、EOF 唤醒待决调用 |
| SDK release worker | DbgEng 10.0.28000.2957，12 组流程通过；`temp/iteration3b-release-regression.log`，工作区 `temp/worker-interaction-05b73f43/` |
| Store release worker | 显式使用 3A 已复制 runtime 10.0.29680.1004，14 组流程通过，包括 MCP 中断和 TTD；`temp/iteration3b-release-store.log`，工作区 `temp/worker-interaction-826587c7/` |
| release 构建 | `script/build-release-install.ps1 -BuildOnly` 成功，MSVC payload 完整：dbgatlas.exe、dbgatlas-worker.exe、dbgatlas_dbgeng.dll、dbgatlas_etw.dll；`temp/iteration3-release-build.log` |

真实 worker 流程覆盖：launch → g → busy 拒绝 → ACTIVE → 停止后查询；旧 epoch 拒绝；运行中 EXIT/close 保留目标；默认 noninvasive attach；DebugBreak 与目标退出事件；原生长循环取消及部分输出保留；超过 64 KiB 的原始输出期间取消；配合/不配合扩展；interrupt/close 竞争；worker 被终止后连接状态与已确认自启目标清理；Dump；TTD 定位、回放和 Offline 状态；事件/HRESULT 文件落盘。

测试只创建合成目标。测试脚本持有并验证进程创建时间的句柄，在结束时清理自建目标；未按进程名批量终止。Dump 使用此前应用户要求生成的下载目录文件，TTD 使用 3A 的短 trace，不修改用户已打开的 WinDbg 会话。

## 本轮发现并修复

1. FILE_FLAG_OVERLAPPED 管道直接交给 Rust File::read，在无数据时触发运行时中止。现在每次读写持有自己的 OVERLAPPED/event，直到完成才释放；连接超时取消后也等待 I/O 完成。
2. EXIT 返回 E_PENDING 后，DbgEng 可呈现 BREAK 而目标仍运行。此时读取当前线程/栈帧曾在 SDK dbgeng.dll 中触发 `0xC0000005`。改为更新缓存中的目标未知状态，不调用 scope/thread 查询，再由所有者 detach；真实关闭和竞争用例通过。
3. 取消后 outer Execute 的结果不能由控制 ack 推断。配合扩展返回内部 E_ABORT 时外层仍可 S_OK；不配合扩展继续成功。这两种情况都保留成功，只有后端明确中断的原生循环被结算 canceled。

## 复现入口

```powershell
cmake -S tests/fixtures/interaction -B temp/interaction-fixture -A x64
cmake --build temp/interaction-fixture --config Release
.\script\build-release-install.ps1 -BuildOnly
python script/test-worker-interaction.py --fixture temp/interaction-fixture/Release --binary target/release/dbgatlas.exe
```

脚本可传 `--dump <绝对路径>`。TTD 如需指定可加载的 runtime，先运行 `cargo build -p dbgatlas-service --release --example interaction-host`，再增加 `--runtime <DbgEng目录> --ttd <run文件>`。该 example 仅启动测试用服务并选择明确的 runtime/worker，不安装服务或修改机器运行配置。脚本将证据留在唯一的仓库 temp 子目录，符号路径限定本地以避免下载耗时干扰控制验证。

## 尚未验收与后续边界

- 本机 `Get-Service *dbgatlas*` 无结果，连接工具清单也无 DbgAtlas MCP。因此没有执行安装态 `service.update`、重启后的 `service.health` 或 Codex 已连接客户端冒烟；独立 HTTP MCP 测试不替代这一验收。
- 3C 尚未实现：服务持有 Execution、wait_ms 有限观察等待、公共 session.open/exec/read 和 execution.*、Artifact 来源/输入引用、执行结果归档失败的完整读取。旧 debug.eval 显式 timeout 仍有杀 worker 语义。
- MCP 产品版本仍为既有 2024-11-05；3A 选择的 2025-06-18 首版适配属于 3D，未通过改版本字符串冒充支持。未声明 Tasks，未测试真实 AI 自动续接或客户端断开后的 Execution 查询闭环。
- 回调队列有界并上报 gap；本轮大输出测试验证控制可达及原始文件保留，未进行长期压力、慢盘/磁盘耗尽、input callback 等待交互或多目标压力验收。
- ETW/TTD 录制代码走工作区回归，未在本批重新执行管理员录制、CPU sampling 或安装服务身份测试。

结论：3B 的执行侧实现与本机回归完成，可进入 3C；安装态验收保留为明确缺口，不将整轮迭代 3 标记完成。
