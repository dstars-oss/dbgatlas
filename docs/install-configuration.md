# 安装配置指南

本文面向已经拿到 DbgAtlas MSI 或 release payload 的本机安装场景，并给出 Codex 与 Claude Code 连接 DbgAtlas MCP endpoint 的参考配置。

## 适用范围

- 操作系统：Windows x64。
- 安装形态：MSI 安装包，或从 `target\release` payload 注册 Windows service。
- 默认安装根：`%ProgramFiles%\DbgAtlas`。
- 默认监听地址：`127.0.0.1:7331`。
- HTTP endpoint：
  - JSON-RPC：`http://127.0.0.1:7331/rpc`
  - MCP：`http://127.0.0.1:7331/mcp`
- 认证方式：`Authorization: Bearer <token>`，token 默认保存在 `<install-root>\etc\token`，并优先通过本机 `DBGATLAS_TOKEN` 环境变量供客户端读取。

DbgAtlas service 只允许 loopback bind。不要把默认 endpoint 暴露到公网，也不要把 bearer token 写入仓库、截图、日志或共享配置。

## 安装

使用当前仓库构建出的 MSI：

```powershell
msiexec.exe /i .\target\installer\DbgAtlas-0.1.0-x64.msi
```

静默安装示例：

```powershell
msiexec.exe /i .\target\installer\DbgAtlas-0.1.0-x64.msi /qn /norestart
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

## Token 配置

安装态 token 是机器本地密钥，不属于 `runtime.toml`。安装逻辑会优先复用机器级 `DBGATLAS_TOKEN`，其次复用已有 `<install-root>\etc\token`，否则生成新 token 并写入安装态 token 文件。

当前用户 shell 临时加载 token：

```powershell
$env:DBGATLAS_TOKEN = (Get-Content "$env:ProgramFiles\DbgAtlas\etc\token" -Raw).Trim()
```

如果希望 Codex 或 Claude Code 从普通用户会话读取同一个 token，可把 token 写入用户级环境变量。不要在终端历史或文档中展开 token 明文：

```powershell
$token = (Get-Content "$env:ProgramFiles\DbgAtlas\etc\token" -Raw).Trim()
[Environment]::SetEnvironmentVariable("DBGATLAS_TOKEN", $token, "User")
```

更新用户级环境变量后，重启 Codex、Claude Code、IDE 或终端，使新环境变量进入客户端进程。

## Codex 配置

Codex 的 MCP 配置写入 `~\.codex\config.toml`，也可以在受信任项目中写入项目级 `.codex\config.toml`。本仓库已 ignore `.codex\`，本机 token 不应提交。

推荐配置：

```toml
[mcp_servers.dbgatlas]
url = "http://127.0.0.1:7331/mcp"
bearer_token_env_var = "DBGATLAS_TOKEN"
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

- 启动 Codex 的进程环境中是否有 `DBGATLAS_TOKEN`。
- `dbgatlas service health --json` 是否成功。
- `http://127.0.0.1:7331/mcp` 是否与实际 `runtime.toml` 中的 `server.bind` 一致。
- 是否在未信任项目中依赖了项目级 `.codex\config.toml`。

开发态 service 可用：

```powershell
cargo run -p dbgatlas-cli -- service run --bind 127.0.0.1:7331 --token dev-token
$env:DBGATLAS_TOKEN = "dev-token"
```

## Claude Code 配置

Claude Code 可直接添加本机 HTTP MCP server。推荐使用本机 loopback endpoint，而不是 Claude 的 cloud custom connector。

临时 shell 中先加载 token：

```powershell
$env:DBGATLAS_TOKEN = (Get-Content "$env:ProgramFiles\DbgAtlas\etc\token" -Raw).Trim()
```

使用 CLI 添加：

```powershell
claude mcp add --transport http dbgatlas http://127.0.0.1:7331/mcp --header "Authorization: Bearer $env:DBGATLAS_TOKEN"
claude mcp list
```

这种方式适合个人本机配置；header 会进入 Claude Code 的本地 MCP 配置状态，应按密钥处理。

如果要把配置放在项目级 `.mcp.json`，可使用 `headersHelper` 让 Claude Code 连接时从环境变量生成 header，避免把 token 明文写入文件：

```json
{
  "mcpServers": {
    "dbgatlas": {
      "type": "http",
      "url": "http://127.0.0.1:7331/mcp",
      "headersHelper": "powershell -NoProfile -Command \"$t=$env:DBGATLAS_TOKEN; if ([string]::IsNullOrWhiteSpace($t)) { throw 'DBGATLAS_TOKEN is not set' }; @{ Authorization = \\\"Bearer $t\\\" } | ConvertTo-Json -Compress\""
    }
  }
}
```

项目级 `.mcp.json` 可以提交给团队，但不要提交任何静态 token。首次使用项目级 MCP server 时，Claude Code 会要求确认项目配置。

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
Test-Path "$env:ProgramFiles\DbgAtlas\etc\token"
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

- `missing or invalid bearer token`：客户端没有发送 `Authorization: Bearer ...`，或 `DBGATLAS_TOKEN` 不是安装态 token。
- 客户端看不到 MCP tools：确认 service health、MCP URL、token、客户端重启和项目 trust 状态。
- Claude Desktop custom connector 连接 `127.0.0.1` 失败：remote custom connector 从 Anthropic cloud 发起连接，无法访问本机 loopback。
- 修改 `runtime.toml` 后未生效：重启 `DbgAtlas` Windows service。

## 参考

- [DbgAtlas runtime model](runtime-model.md)
- [Codex MCP](https://developers.openai.com/codex/mcp)
- [Codex config basics](https://developers.openai.com/codex/config-basic)
- [Claude Code MCP](https://code.claude.com/docs/en/mcp)
- [Claude custom connectors using remote MCP](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp)
