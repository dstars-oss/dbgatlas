# 本机 HTTP 认证移除验证记录

验证日期：2026-11-14。范围：完整删除本机 `/rpc`、`/mcp` 的 bearer token 认证及其配置、安装和 CLI 依赖，不提供兼容开关或参数别名。

## 行为

- 服务只绑定 loopback，保留浏览器 `Origin` 校验；本机调用无需认证。
- CLI 不再接受认证参数，安装配置发现和 SCM 服务启动不再读取凭据文件。
- 安装器不再生成 token、写入机器环境变量或管理凭据文件；移除专属依赖和配置示例。
- `service.update` 将 SCM 启动命令同步为当前版本参数，保留服务账户、启动类型、服务类型、错误控制、显示名和依赖。
- Windows worker 身份切换与命名管道 ACL 未修改。安装态服务为 LocalSystem，本机调用者可以使用调试及更新能力；loopback 和 `Origin` 校验不提供本机用户隔离。

## 验证结果

| 检查 | 结果 |
| --- | --- |
| `cargo test -p dbgatlas-service -p dbgatlas-cli` | 131 passed，0 failed；覆盖无凭据工作流、非法 Origin 和已移除 CLI 参数拒绝 |
| `script/build-release-install.ps1 -BuildOnly` | MSVC release 构建成功，四文件 payload 完整 |
| 独立只读审查及修正后复查 | 通过；一处文档残留已修正 |
| MCP `service.update` | 使用仓库绝对 `target/release` 路径，`restart: true`，更新成功 |
| SCM 状态 | Running，LocalSystem，Auto；启动命令不含已移除参数 |
| MCP `service.health` 与安装态 CLI health | 均返回 `ok` |
| 安装 payload | 四文件 SHA-256 与 release 一致 |
| 无 Authorization 的 HTTP 请求 | `/rpc` health、`/mcp` initialize、tools/list 均 HTTP 200；26 项工具 |
| 无 Authorization、外部 Origin 的请求 | `/rpc` 与 `/mcp` 均 HTTP 403 |
| MCP 实际调试 | launch → `.echo auth-removal-smoke` → close 均成功 |
| `git diff --check` | 通过 |

调试会话：`session-1794634597681-1`。操作：创建 `op-1794634597681-1`，执行 `op-1794634601567-2`，关闭 `op-1794634601618-3`。命令原始输出引用：`artifact-1794634601578-2`。

本地证据保存在仓库 `temp/`，不提交：`auth-removal-tests.log`、`auth-removal-release.log`、`auth-removal-http-smoke.json`、`auth-removal-service-before.json`、`auth-removal-service-after.json`。调试工作区为 `temp/auth-removal-smoke/`；测试 `TEMP`/`TMP` 指向 `temp/test-scratch/`。release 构建使用 Visual Studio 自带 CMake 与 MSVC Rust toolchain。

项目本机 `.codex/config.toml` 已去除认证环境变量配置，继续保持 ignored。已有安装的废弃凭据文件或环境变量不参与新版运行，本次没有额外清理这些本机残留，也未引入其迁移逻辑。

## 未覆盖

未执行新 MSI 构建及全新安装；本轮未重复验证 ETW、TTD、dump/attach 或远端后端。当前结果证明本机入口移除认证及代表性调试链路可用。
