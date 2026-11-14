# 安装配置指南

本文面向已经拿到 DbgAtlas MSI 或 release payload 的本机安装场景，并给出 Codex 与 Claude Code 连接 DbgAtlas MCP endpoint 的参考配置。

## 适用范围

- 操作系统：Windows x64。
- 安装形态：MSI 安装包，或从 `target\release` payload 注册 Windows service。
- 默认安装根：`%ProgramFiles%\DbgAtlas`。
- 默认监听地址：`127.0.0.1:7331`。
- 服务启动类型：自动启动（`AUTO_START`），Windows 启动时由 SCM 启动，不依赖用户登录。MSI、安装脚本和 `service install --force` 均设置此默认值。
- HTTP endpoint：
  - JSON-RPC：`http://127.0.0.1:7331/rpc`
  - MCP：`http://127.0.0.1:7331/mcp`
- 访问方式：无需认证，信任本机调用者；保留 loopback bind 和浏览器 `Origin` 校验。

DbgAtlas service 只允许 loopback bind。安装态服务以 LocalSystem 运行，本机调用者可以使用调试、录制和服务更新能力；此配置不提供本机用户或进程隔离。

## 安装

使用当前仓库构建出的 MSI：

```powershell
.\script\build-msi.ps1
msiexec.exe /i .\target\installer\DbgAtlas-0.3.0-x64.msi
```

静默安装示例：

```powershell
msiexec.exe /i .\target\installer\DbgAtlas-0.3.0-x64.msi /qn /norestart
```

安装完成后检查服务：

```powershell
dbgatlas service status --json
dbgatlas service health --json
```

如果 `dbgatlas` 不在当前 shell 的 `PATH` 中，可使用安装路径：

```powershell
& "$env:ProgramFiles\DbgAtlas\bin\dbgatlas.exe" service health --json
```

如需从 release payload 直接安装，使用管理员 PowerShell：

```powershell
.\script\build-release-install.ps1 -BuildOnly
.\target\release\dbgatlas.exe service install --payload-mode copy --payload-dir .\target\release --force
.\target\release\dbgatlas.exe service start
.\target\release\dbgatlas.exe service health --json
```

`service install` 注册开机自动启动，但不立即启动服务；上面的 `service start` 用于本次安装后立即启动。MSI 和 `build-release-install.ps1` 默认会执行启动步骤；脚本的 `-NoStart` 仅跳过本次启动，不关闭开机自动启动。

已有安装若仍为手动启动，可在管理员 PowerShell 中修正并立即启动：

```powershell
Set-Service -Name DbgAtlas -StartupType Automatic
Start-Service -Name DbgAtlas
```

`service.update` 更新 payload 和当前版本的 SCM 启动命令，并按请求重启；保留现有服务账户、启动类型和依赖。

## Codex 配置

Codex 的 MCP 配置写入 `~\.codex\config.toml`，也可以在受信任项目中写入项目级 `.codex\config.toml`。本仓库已 ignore `.codex\`，本机配置不提交。

推荐配置：

```toml
[mcp_servers.dbgatlas]
url = "http://127.0.0.1:7331/mcp"
enabled = true
startup_timeout_sec = 10
tool_timeout_sec = 120
```

验证方式：

```powershell
codex
```

在 Codex TUI 中执行：

```text
/mcp
```

应能看到 `dbgatlas` server 及其工具。如果没有出现，优先检查：

- `dbgatlas service health --json` 是否成功。
- `http://127.0.0.1:7331/mcp` 是否与实际 `runtime.toml` 中的 `server.bind` 一致。
- 是否在未信任项目中依赖了项目级 `.codex\config.toml`。

开发态 service 可用：

```powershell
cargo run -p dbgatlas-cli -- service run --bind 127.0.0.1:7331
```

## Claude Code 配置

Claude Code 可直接添加本机 HTTP MCP server。推荐使用本机 loopback endpoint，而不是 Claude 的 cloud custom connector。

使用 CLI 添加：

```powershell
claude mcp add --transport http dbgatlas http://127.0.0.1:7331/mcp
claude mcp list
```

项目级 `.mcp.json` 的顶层使用 `mcpServers`，只需配置 endpoint：

```json
{
  "mcpServers": {
    "dbgatlas": {
      "type": "http",
      "url": "http://127.0.0.1:7331/mcp"
    }
  }
}
```

项目级 `.mcp.json` 可以提交给团队。首次使用项目级 MCP server 时，Claude Code 会要求确认项目配置。

在 Claude Code 中验证：

```text
/mcp
```

如需查看配置细节：

```powershell
claude mcp get dbgatlas
```

## Claude Desktop 与 claude.ai 注意事项

Claude Desktop / claude.ai 的 custom connector 使用远程 MCP 时，请注意连接来自 Anthropic cloud infrastructure，不是来自本机进程。因此默认的 `http://127.0.0.1:7331/mcp` 只适合本机客户端，例如 Codex 和 Claude Code；它不能作为 claude.ai 或 Claude Desktop remote custom connector 的 URL。

若确实要让 Claude Desktop 或 claude.ai 通过 remote connector 访问 DbgAtlas，需要额外设计公开可达的 HTTPS endpoint、OAuth 或等价认证、访问控制、审计和网络隔离。当前 DbgAtlas 默认安装配置不建议这样部署。

Claude Desktop 也存在本地 MCP 配置机制，但其行为和可用 transport 取决于 Claude Desktop 当前版本。对本仓库的默认本机调试工作流，优先使用 Claude Code 的 HTTP MCP 配置。

## 排障

检查安装态文件：

```powershell
Test-Path "$env:ProgramFiles\DbgAtlas\bin\dbgatlas.exe"
Test-Path "$env:ProgramFiles\DbgAtlas\etc\runtime.toml"
```

检查服务健康：

```powershell
& "$env:ProgramFiles\DbgAtlas\bin\dbgatlas.exe" service health --json
```

检查监听端口：

```powershell
Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort 7331 -ErrorAction SilentlyContinue
```

查看 service 日志：

```powershell
Get-ChildItem "$env:ProgramFiles\DbgAtlas\var\log" -Filter "service-*.log" |
  Sort-Object LastWriteTime -Descending |
  Select-Object -First 1 |
  Get-Content -Tail 200
```

常见问题：

- 客户端看不到 MCP tools：确认 service health、MCP URL、客户端重启和项目 trust 状态。
- Claude Desktop custom connector 连接 `127.0.0.1` 失败：remote custom connector 从 Anthropic cloud 发起连接，无法访问本机 loopback。
- 修改 `runtime.toml` 后未生效：重启 `DbgAtlas` Windows service。

## 参考

- [DbgAtlas runtime model](runtime-model.md)
- [Codex MCP](https://developers.openai.com/codex/mcp)
- [Codex config basics](https://developers.openai.com/codex/config-basic)
- [Claude Code MCP](https://code.claude.com/docs/en/mcp)
- [Claude custom connectors using remote MCP](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp)
