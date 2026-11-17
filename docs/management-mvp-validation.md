# 管理页面 M1 验证记录

日期：2026-11-17。基于 `751cf0f` 之后的本地管理页面 MVP 改动。迭代 4/5 并行推进，M1 先使用既有 JSONL 与产物文件；SQLite 元数据存储未实现。

## 实现范围

- 同一 DbgAtlas 服务进程增加独立管理 listener，默认 `127.0.0.1:7332`；RPC/MCP 保留 `7331`。CLI 支持 `--management-bind` / `--no-management`，安装配置支持 `server.management_bind` / `management_enabled`。
- 内嵌中文 HTML/CSS/JavaScript 页面，提供项目目录选择、会话与操作列表、状态/文本筛选、分页、操作上下文、产物和原始输出查看；最近项目保存在浏览器本地。
- 查询复用 Workspace JSONL、session.json 和服务运行期事实，完整输出继续保留文件。历史非终态不当成当前运行态；已知执行结果优先于旧日志，归档错误单独表达。
- 管理接口只读，校验本机 Host 与同源浏览器请求，不转发通用 RPC；按登记的 Artifact ID 受控读取文件，每次文本读取最多 64 KiB。

## 验证结果

| 层次 | 结果 |
| --- | --- |
| 编译与静态检查 | `cargo check -p dbgatlas-service -p dbgatlas-cli -p dbgatlas-runtime`、JavaScript 语法检查通过 |
| 相关回归 | `cargo test -p dbgatlas-service -p dbgatlas-cli -p dbgatlas-runtime` 共 168 项通过；新增 4 项 management 测试及 1 项 runtime 配置测试 |
| 状态修正回归 | 独立审查指出运行期结果应优先于旧 JSONL；增加最终归档失败的 success + output_error 断言后，4 项 management 测试再次通过 |
| Release | 维护脚本 `script/build-release-install.ps1 -BuildOnly` 使用 VS 2026 / MSVC 构建成功，四文件 payload 完整 |
| 安装态更新 | 通过连接的 MCP `service.update(source_dir=D:\Repos\Project\dbgatlas\target\release,restart=true)` 更新；health 为 0.4.0/ok，安装文件与 release SHA-256 一致 |
| 实际 MCP 调试 | active_interactive_user 启动合成 cmd.exe 目标，执行 `.echo`、`r`、`lm`，正常关闭；5 条操作、11 个 Artifact 可从管理端读取，命令 marker 一致 |
| 安装态 HTTP | localhost:7332 返回页面；可读取此前 workspace 的 4 个历史会话和 36 条操作；管理端 /rpc、/mcp 返回 404，外部及其他端口 Origin 返回 403 |
| 实际浏览器 | 本机 Chrome 无界面模式 6 组检查通过：真实记录、详情输出、文本筛选、状态空结果、无效项目清空旧数据、390 px 页面无横向溢出；无 JavaScript 异常 |
| 视觉检查 | 已查看 1440 px 桌面、详情和 390 px 窄屏截图；表格在窄屏容器内可横向滚动 |
| 独立只读审查 | 修正项目切换时旧数据与新路径混用、已结束操作显示旧 running 两项问题；复审无遗留阻断项 |

MCP 冒烟会话为 `session-1794889672774-1`，marker Execution 为 `op-1794889681184-3`。目标为 `cmd.exe /c exit 0`，正常关闭后自然退出，最终活动会话为 0。

本机证据位于 `temp/management-mvp-tests/`：`narrow-tests.log`、`release-build*.log`、`payload-hashes.json`、`mcp-evidence.json`、`installed-http-summary.json`、`browser-summary.json`、`desktop.png`、`detail.png`、`mobile.png` 及测试 workspace。一次性探针、Chrome profile 和格式化工具缓存均留在 repo temp，不提交。

## 验证边界

可视桌面浏览器工具因 `GetCursorPos failed: 拒绝访问。 (0x80070005)` 无法初始化；改用本机 Chrome headless 访问真实安装服务，执行页面交互并截图，未宣称在用户可视窗口中打开成功。

本轮没有执行全工作区测试、重新录制 ETW/TTD 或大规模历史性能测试。SQLite、幂等迁移、全文检索、时间筛选、跨重启运行现场恢复和网页命令控制不属于本轮已实现能力。API 查询分页前仍扫描 JSONL，性能优化随 M2 存储迁移推进。

建议将本轮已验证的管理端 HTTP/浏览器探针整理为可指定安装 endpoint 的维护脚本，后续共同验收记录与页面变化；现有客户端/worker 测试脚本已增加 `--no-management`，避免与安装态管理端口冲突。
