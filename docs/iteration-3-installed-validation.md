# 迭代 3：安装态验收记录

日期：2026-11-16。实现提交：`751cf0f`，API/包版本 0.4.0、worker v9、DbgEng ABI 0.7.0。本轮补齐 3A—3D 本机验证之后的安装态更新、健康检查与代表性 MCP 冒烟；未修改产品代码。

## 构建与更新

- 更新前：DbgAtlas SCM 服务以 LocalSystem 运行，连接的 MCP health 返回 0.3.0，活动会话为 0。
- Rust：`rustc 1.96.0`，host 为 `x86_64-pc-windows-msvc`。构建进程优先使用 VS 2026 自带的 CMake 4.3.1-msvc1；native generator 为 `Visual Studio 18 2026`，fixture 编译器为 MSVC 19.51.36260.0。
- 从仓库根目录运行 `script/build-release-install.ps1 -BuildOnly`，release 构建成功，四文件 payload 完整。
- 通过本轮 Codex 已连接的 DbgAtlas MCP 调用 `service.update`，参数为 `source_dir=D:\Repos\Project\dbgatlas\target\release`、`restart=true`。服务接受更新后重启，MCP `service.health` 返回 `status=ok`、`version=0.4.0`。
- 更新后 SCM 进程 PID 为 32568，程序路径为 `C:\Program Files\DbgAtlas\bin\dbgatlas.exe`，账户仍为 LocalSystem。四个安装文件的 SHA-256 均与本次 release 相同。

| 文件 | release / 安装态共同 SHA-256 |
| --- | --- |
| dbgatlas.exe | `DB71D3319767FC3E011820A14301BA942E919B257D7370256B7CA86A3A40FC2B` |
| dbgatlas-worker.exe | `06DF3A84CAF3EAF6159EC0F09DD760611CB97DB5F5FA388ECBADFD71D2B627A4` |
| dbgatlas_dbgeng.dll | `A891649DCDAE2836012032F060DF1DEB11A7B629550AEE66A31BEB614D401F42` |
| dbgatlas_etw.dll | `C625EC8BAEC7ADFDF0C74EFBC1054808EF6DCBA7A450148D1AE57F61CE738F38` |

首次构建使用 PATH 中 Strawberry 附带的旧 CMake，选择 VS 2022 后因本机没有该实例失败；仅调整本次构建进程的 PATH 后重跑成功，没有改动全局环境或使用 MinGW/UCRT 编译产物。

## MCP 冒烟

所有业务探针连接安装态 `http://127.0.0.1:7331/mcp`，未启动替代开发服务。fixture 来自 `tests/fixtures/interaction`，工作进程使用 `active_interactive_user`，目标为本轮生成的合成程序。

本轮连接工具仍暴露更新前的工具元数据，因此实际 Codex 工具调用验证了 `service.update`、`service.health`、兼容 `debug.session.create`、`operation.get`、`debug.eval` 和 `session.list`。首次打开在默认等待预算到期后返回 running，随后查询确认成功，原生命令返回预期 marker。新的 canonical 接口通过同一安装服务的 HTTP MCP `tools/call` 验证。

最终脚本运行 8 组检查全部通过：

| 场景 | 观察结果 |
| --- | --- |
| MCP 协商与发现 | 三个支持版本均协商成功；默认工具为 25 项，包含 session.exec/execution.read，隐藏 debug.eval；新版本返回 structuredContent |
| 打开与短命令 | session.open 成功，session.exec 执行原生命令，execution.get/read 返回成功和预期文本 |
| 长命令与有限等待 | streaming 扩展在 10 ms 观察预算后仍继续；相同请求键返回同一 Execution，忙时新命令被拒绝，实时/归档输出均可读，worker 身份未变化 |
| 客户端断线 | 请求被观察为 executing 后关闭 HTTP 连接；通过原请求键核对同一执行，最终 success，保留 stream:end |
| 输出分页 | 80 行合成输出使用 64 字节预算完整分页读取，内容数量一致且没有 gap |
| 取消与目标中断 | 无限原生命令经 execution.cancel 后最终 canceled；g 后等待事件，再通过 session.interrupt 回到 idle |
| 正常关闭 | session.close 返回 closed；保留句柄确认目标仍存活；关闭后 execution.read、session.read 和 execution.list 可用 |
| 断点停止 | 新会话继续执行 fixture，异步输出记录 `Break instruction exception - code 80000003`，引擎事件记录对应异常；目标 stopped，可读取 RIP 后正常关闭 |

最终成功运行的会话为 `session-1794815680516-3` 和 `session-1794815687150-4`，打开 Execution 分别为 `op-1794815680516-22` 和 `op-1794815687150-35`。服务实例为 `service-32568-1794815517689`。

前两次脚本运行分别因测试断言错误停止：将 DbgEng 的 `80` 当作十进制，以及假设取消响应含 `accepted` 字段。探针改用 `0n80`，并分别检查 `control_execution.status` 和原 Execution 的 canceled 终态后，完整重跑通过；失败记录保留，不计作完整通过。产品代码未因此修改。

结束时通过连接的 MCP 再次确认 health 为 0.4.0/ok，活动会话为 0；进程查询没有 probe_target.exe 或 dbgatlas-worker.exe。探针仅通过核对 creation time 后保留的进程句柄清理自身目标。

## 证据与边界

本机证据根：`temp/iteration3-installed-20261116/`（不提交）。包含：

- `release-build.log`、`release-build-msvc.log`：首次失败与最终成功的构建日志。
- `connected-mcp.json`、`connected-current.json`、`final-connected-mcp.json`：连接工具的更新、打开和最终检查结果。
- `payload-hashes.json`、`service.json`：安装文件与 SCM 状态。
- `smoke.py`、`summary.json`、`mcp-*.json`：最终探针、8 组通过摘要与各次 MCP 请求/响应。
- `breakpoint-events.json`、`closed-history.json`、`workspace/`：断点事件、关闭后历史与原始产物。
- `first-attempt-summary.json`、`second-attempt-summary.json`：测试断言修正前的运行记录。

本轮未重跑 245 项工作区测试；该结果仍属于 [3D 验证](iteration-3d-validation.md)。本轮构建与安装态冒烟已实际执行。未重新验证 Dump/TTD 回放、ETW CPU/TTD 录制、默认 LocalSystem worker 调试、invasive attach、其他 MCP 客户端或并发模型调度；既有覆盖仍以各阶段记录为准。服务重启后恢复活动现场、SQLite、远端、Tasks/SSE 等不属于当前能力。

建议将现有交互脚本扩展为可显式连接既有安装 endpoint，并在构建前检查 CMake 与已安装 VS generator 是否匹配；本轮临时探针和 PATH 调整不改变维护脚本行为。
