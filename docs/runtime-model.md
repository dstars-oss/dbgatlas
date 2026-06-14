# Runtime Model

DbgAtlas 区分 analysis workspace 和 runtime install/config。workspace 保存项目事实；runtime config 保存本机工具、进程和网络策略。

## Analysis Workspace

analysis workspace 是显式目录，包含：

```text
analysis-workspace/
  dbgatlas-workspace.json
  artifacts/
  analysis/
  inputs/
```

`dbgatlas-workspace.json` 只记录 workspace identity、schema version 和创建信息，不记录本机 DbgEng、IDA、proxy 或 service install root。

对外 service API 不暴露 workspace 资源。调用方在创建 session 时传入 `project_root`，service 内部固定使用 `<project_root>/dbgatlas` 作为 analysis workspace；该目录是可见目录，不使用隐藏 `.dbgatlas`。

## Runtime Config

`dbgatlas-runtime` 第一版只定义类型、解析入口和校验。配置项包括：

- `tools.dbgeng_dir`
- `tools.symbol_path`
- `tools.ttd_dir`
- `tools.ida.install_dir`
- `tools.ida.python_executable`
- `tools.ida.vendor_src_dir`
- `process.child_identity`
- `process.fallback_child_identity`
- `process.elevate_if_admin`
- `proxy`
- `server.bind`

MVP 0.5 不安装 Windows service，不启动 worker，也不把 runtime config 写入 workspace。

## 校验原则

- 本地服务默认只绑定 loopback。
- 安装态服务配置和机器级 bearer token 默认属于 `%ProgramData%\DbgAtlas\`。
- 对外 HTTP API 需要 bearer token，并校验来自浏览器类客户端的 `Origin`。
- path、symbol path、proxy value 拒绝控制字符。
- proxy env 只允许明确支持的 key。
- 运行时配置可随机器变化；workspace artifacts 必须保持可审计和可复现。
