# 迭代 2 验证记录

日期：2026-11-14。范围：会话/操作公共模型、专用 DbgEng/ETW/TTD worker、本机进程归属与 JSONL 兼容。历史验证记录保持不变。

## 契约与回归

- 最终全工作区 `cargo test --workspace`：228 项通过，0 项失败。开发过程中先执行受影响 crate/native 边界窄范围测试，修正后再执行工作区回归。
- 覆盖固定绑定、终态摘要、录制双 ID、忙时拒绝、取消接受与执行结果分离、输出登记失败、旧 JSONL 合并、PID 复用/未知身份/非本机环境保护、正常释放目标后的工具清理、CPU interval 恢复失败。
- 最终测试日志：`temp/iteration2-workspace-tests-final.log`；针对性日志：`temp/iteration2-final-narrow.log`、`temp/iteration2-process-tests.log`、`temp/iteration2-target-exit-test.log`。
- 独立只读审查及修正后复查：未发现阻断项。修正了 helper 注册竞态、native launch 提交时机、异常 RPC 退出清理、CPU interval 恢复确认、多类清理错误聚合、并发终止确认及无 backend snapshot 时的目标退出监测。

## 开发态实测

测试工具和数据均在 `temp/iteration2-smoke/`，使用独立 loopback 服务、MSVC 编译的测试目标，不对安装态服务做故障注入。

- launch 正常关闭后 worker 退出、目标存活；两个 session 使用不同 worker；单 worker 崩溃不影响其他会话，已登记自启目标被清理。
- attach 的正常关闭和强制关闭均保留既有测试进程。
- ETW start/stop、worker 崩溃后的残留采集核对与清理通过。
- 模拟开发态服务崩溃后，worker 和本会话自启测试目标退出。
- 非管理员开发态 CPU sampling/TTD 受到权限限制；不能把其失败算作后端成功。安装态单独验证。
- 同一 Dump 的两次独立打开、不同 worker 和原生命令通过。带符号的栈查询曾等待外部符号，最终隔离验证使用无需符号的 `.echo`。
- ETW 附加的短生命周期目标自然退出后，`session.get` 返回会话 active、目标 exited、来源 process_monitor；随后正常停止成功。证据：`temp/iteration2-smoke/target-exit-evidence.json`。
- TTD helper 真实子进程退出码 0/37 原样返回；开发态 TTD 权限失败保留实际非零退出码和 `output_complete=false`。证据：`temp/iteration2-installed/helper-exit-test.py`、开发态 `rpc-evidence.json`。

原始请求/响应及断言：`temp/iteration2-smoke/rpc-evidence.json`、`checks.json`、`run.log`。中间失败轮次不作为最终通过证据。

## Release 与安装态

MSVC release payload 由 `script/build-release-install.ps1 -BuildOnly` 构建，最终日志 `temp/iteration2-release-final.log`。通过已连接 MCP 调用 `service.update`，参数为当前仓库 `target/release` 绝对路径及 `restart: true`；重启后 MCP `service.health` 返回 ok。四个安装文件与构建产物 SHA-256 一致，清单：`temp/iteration2-installed-hashes.json`。

| 场景 | 结果与代表性标识 |
| --- | --- |
| Debug launch → 原生命令 → close | `session-1794650309067-1`；worker 退出，自启目标保留，cleanup.complete=true |
| Dump → 原生命令 → close | `session-1794650455950-1`；离线状态、精确进程实例字符串及关闭确认通过 |
| ETW CPU start → stop | 最终 payload 的 `recording-1794651238348-2`；成功并登记 5 个产物引用，worker 退出，附加目标保留 |
| TTD launch → 时长停止 → 归档 | 最终 payload 的 `recording-1794651213775-1`；success、output_complete=true、cleanup.complete=true，正常 Job 释放后目标存活 |
| TTD replay → 原生命令 → close | `session-1794651237843-1`；离线会话、`.echo iteration2-verified` 和关闭完成 |
| TTD 录制中并发 kill | 独立 HTTP RPC 连接同时执行 recording.ttd/recording.kill；worker、helper、recorder 和已登记自启目标全部退出，会话 failed、cleanup.complete=true，原操作不虚报成功 |

前三类后端代表性链路均通过已连接 MCP 执行；并发强制关闭通过同一安装态业务 API 的独立 HTTP 连接执行，避免 MCP 客户端串行派发掩盖并发行为。未对安装态服务注入进程崩溃。

原始证据在 `temp/iteration2-installed/`：`mcp-evidence.json` 保留早期代表性请求结果，`mcp-release-evidence.json` 保存最终 payload 的 TTD/回放/CPU 结果，`forced-ttd-evidence.json` 保存并发请求、关闭前所有权清单和进程退出断言。测试目标、输入和产物均属于本轮临时工作区，不作为仓库提交内容。

## 验证边界

- 本轮未实际覆盖 TTD monitor、TTD active_interactive_user attach、内核调试、所有候选 runtime/身份失败组合，以及带活动长操作的 SCM 正常停服超时升级场景；对应契约或模拟测试不替代后端实测。
- 非管理员开发态 CPU/TTD 权限失败保留在检查结果中；安装态 LocalSystem 的实际成功链路单独记录，不改写失败结果。
- Debug `g` 后的完整事件驱动执行/退出消费不作为本轮通过项；目标退出的真实进程监测验证使用 ETW 目标。完整事件消费、可靠 interrupt、MCP 长任务和新的等待预算语义在迭代 3 深化。
- 没有真实既有调试器接入、远端传输、服务重启恢复或完整多进程调试能力。未知启动目标的故障标记保守报告清理未完成；不以进程树猜测或裸 PID 代替归属证据。
- 文档相对链接、旧计划文件引用及 `git diff --check` 均检查；历史验证记录未改写。
