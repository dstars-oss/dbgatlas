# 迭代 1 验证记录

验证日期：2026-11-14。范围：IDA 集成剥离及现有本机调试/录制链路。未实现迭代 2 及后续目标。

## 实现与接口

- 删除 `dbgatlas-ida`、`dbgatlas-ida-sys`、native IDA adapter/header、IDA SDK 9.3/9.4 和其专用 nlohmann/json 依赖。
- RPC/MCP 方法注册表精确移除 30 个 `reverse.*` 方法，其余 26 个方法保留。无专用旧接口响应，未注册的方法统一走未知方法处理。
- worker 协议为 v6，不再接受 reverse 请求/响应。服务和 worker 同步更新。
- 原 reverse 支持模块中的两个共享录制响应函数原样移入录制模块。

## 清理后的本地验证

| 检查 | 结果 |
| --- | --- |
| 从空构建缓存运行 `cargo test --workspace` | 225 passed，0 failed，0 ignored |
| `cargo test -p dbgatlas-service -p dbgatlas-workspace -p dbgatlas-runtime -p dbgatlas-model` | 共 157 项，全部通过 |
| `script/build-release-install.ps1 -BuildOnly` | MSVC release 构建及四文件 payload 检查通过 |
| `script/build-msi.ps1 -SkipBuild` | MSI 构建通过；保留既有 VBScript custom action 的 WIX1163 警告 |
| `git diff --check` | 通过 |
| 独立只读审查及修正后复查 | 无遗留问题；确认未删除非 reverse 方法，未误删共享录制逻辑 |

本机 PATH 最先命中的 Strawberry CMake 不支持 VS2026，最初配置失败；验证时将 VS2026 自带 CMake 放到 PATH 首位，使用 Visual Studio 18 2026 / MSVC 构建，未使用 MinGW/UCRT 产物。测试临时目录通过 `TEMP`/`TMP` 指向仓库 `temp/test-runtime/`。

本地日志：`temp/iteration1-cleanup-full-tests.log`、`temp/iteration1-cleanup-tests.log`。MSI：`target/installer/DbgAtlas-0.3.0-x64.msi`。这些生成产物不提交。

## 安装态验证

通过 Codex 连接的 MCP 调用 `service.update`，`source_dir` 为 `D:\Repos\Project\dbgatlas\target\release`，`restart: true`。


重启后 `service.health` 为 `ok`，doctor 全部七项为 `ok`，安装目录 `bin` 的顶层文件仅为：

- `dbgatlas.exe`
- `dbgatlas-worker.exe`
- `dbgatlas_dbgeng.dll`
- `dbgatlas_etw.dll`

四个安装文件与 release 文件 SHA-256 一致，release 无 `dbgatlas_ida*.dll`。WinDbg runtime 子目录保留。

清理后再次更新服务，安装态 `tools/list` 返回 26 个工具、0 个 reverse 工具，结果保存于 `temp/iteration1-cleanup-installed.json`。未注册的旧工具调用返回普通 `unknown DbgAtlas tool` 错误，无专用响应；`artifact.list` 仍能查询到首次冒烟留下的 12 条产物记录。四个安装文件与本轮重新构建的 release 哈希一致，`service.health` 为 `ok`。

## 首次实现时的代表性冒烟

分析 workspace 位于 `temp/iteration1-smoke/dbgatlas/`，所有原始输出由安装态服务生成。

| 路径 | 实际结果与证据 |
| --- | --- |
| 调试 launch → eval → close | 启动 `cmd.exe /c exit 0`，会话 `session-1794629669891-1`；执行 `.echo iteration1-debug-ok; r; k; u @$ip L3`，返回寄存器、栈和指令文本，共 829 字节；关闭状态为 `Closed` |
| 调试输出持久化 | `artifacts/sessions/session-1794629669891-1/raw/op-1794629688861-2.txt`，同时登记 transcript/events 和 operation |
| ETW start → stop | 录制 `cmd.exe /c ping -n 4 127.0.0.1 >nul`，开启 process/thread/image/file；录制 `recording-1794629688911-1` 最终为 `stopped`，无采集/过滤错误 |
| ETW 产物 | `artifacts/recordings/recording-1794629688911-1/trace.etl` 为 67,149 字节；过滤后写入 924 个事件，四类事件 JSONL 均存在；实时消费为事实来源，未运行离线重提取 |
| 持久化查询 | MCP `artifact.list` 返回 12 条产物元数据；`operation.list` 返回 5 条成功操作，覆盖调试创建/执行/关闭和录制开始/停止 |

本轮未运行新 MSI 全新安装、TTD 录制/回放、dump/attach、CPU sampling 和内核调试；上述本机代表性结果不等同于这些路径已完成运行验收。工作区没有真实分析数据清理或格式迁移。

## 后续工程改进

release 脚本可在构建前检查当前 CMake 是否支持已安装的 Visual Studio，并给出明确修复路径；本次通过显式设置 PATH 完成验证。迭代 2 应按实际代码梳理 ETW controller 与文档中 recording worker 的历史描述差异。

## 残留清理复查

- 已删除专用兼容响应、reverse workspace helper、旧产物分类分支、专用配置兼容测试、旧开发计划和迁移指南；相关 AGENTS.md 与现有文档已同步。
- 定向删除后发现 CMake 工程和 Cargo fingerprint 仍引用旧依赖，因此清空仓库 `target/`、`build/` 后重新构建和测试。重新生成的缓存文件名及 CMake/Cargo 配置中，`dbgatlas[_-]ida`、`IDA_SDK`、`nlohmann-json` 均无命中。
- 清理已删除组件的空目录与本次一次性辅助脚本，没有删除分析 workspace、产物或笔记。当前 15 个 workspace crate 的 manifest 无 IDA 依赖。
- 后续此类清理检查应包含 Git 忽略目录、生成配置内容及文件名，不能只依赖默认源码搜索。
