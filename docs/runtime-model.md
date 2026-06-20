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

`dbgatlas-workspace.json` 只记录 workspace identity、schema version 和创建信息，不记录本机 DbgEng、ETW、IDA、proxy 或 service install root。

对外 service API 不暴露 workspace 资源。调用方在创建 session 时传入 `project_root`，service 内部固定使用 `<project_root>/dbgatlas` 作为 analysis workspace；该目录是可见目录，不使用隐藏 `.dbgatlas`。

## Runtime Config

`dbgatlas-runtime` 第一版只定义类型、解析入口和校验。配置项包括：

- `tools.dbgeng_dir`
- `tools.symbol_path`
- `tools.etw.adapter_dir` 或等价 native ETW adapter 位置（MVP 3 规划）
- `tools.etw.default_presets`（MVP 3 规划）
- `tools.ttd_dir`
- `tools.ida.install_dir`
- `tools.ida.python_executable`
- `tools.ida.vendor_src_dir`（历史兼容配置；IDA native adapter 已维护最小自有 ABI 声明，构建和运行时打开 reverse session 均不依赖该 SDK 路径）
- `tools.ida.allow_py_eval`（默认 `false`；显式开启高权限 `reverse.py_eval` / IDAPython 执行能力）
- `process.child_identity`
- `process.fallback_child_identity`
- `process.elevate_if_admin`
- `proxy`
- `server.bind`

安装态 Windows service 使用 `%ProgramData%\DbgAtlas\` 作为机器级 install root：

```text
%ProgramData%\DbgAtlas\
  bin\                  # installed runtime payload
    dbgatlas.exe
    dbgatlas-worker.exe
    dbgatlas_dbgeng.dll
    dbgatlas_etw.dll
    dbgatlas_ida.dll
  etc\
    runtime.toml        # runtime config
    token               # machine-local bearer token
  var\
    log\
      service-YYYY-MM-DD.log
```

SCM 注册的 `DbgAtlas` service 指向 `%ProgramData%\DbgAtlas\bin\dbgatlas.exe`，不指向开发目录下的 `target\debug` 或 `target\release`。`dbgatlas service install` 从当前 executable 所在目录复制 runtime payload；若发现旧布局中的 `%ProgramData%\DbgAtlas\runtime.toml` 或 `%ProgramData%\DbgAtlas\token`，会迁移到 `etc\`。`dbgatlas service uninstall` 默认只删除 `bin\` 和 SCM entry，保留 `etc\` 与 `var\log\`，`--purge` 才删除整个 install root。

安装态 service 还暴露 `service.update` JSON-RPC/MCP 方法，接收一个已经构建好的 payload 目录。该方法只完成校验并启动独立 updater 进程，然后异步返回 accepted；updater 会先复制到 `bin.next-*`，停止服务，使用 rename 将旧 `bin` 移到 `bin.old-*` 并把新 payload 放到 `bin`，最后按请求重启服务并 best-effort 清理旧目录。更新结果写入 service 日志。

安装态 service 日志写入 `%ProgramData%\DbgAtlas\var\log\service-YYYY-MM-DD.log`，按 UTC 日期滚动，保留当天和前 6 天的 service 日志。

开发态 `dbgatlas service run --bind ... --token ...` 仍直接使用当前进程和当前目录，不注册 SCM，也不写入 `%ProgramData%`。同一个 HTTP listener 暴露 `/rpc` 和 `/mcp`；二者复用同一个 bearer token。开发态如需暴露高权限 IDAPython 执行能力，必须显式传入 `--allow-ida-py-eval`。

## Recording Runtime

MVP 3 的 recording runtime config 只保存本机工具和进程策略，不进入 analysis workspace manifest。ETW recording 默认通过受控 worker 调用 C++ ETW adapter，adapter 负责 ETW session、provider enable、实时消费、过滤和 flush。

recording policy 的文档目标：

- 默认使用内置 process/thread/image/file/registry/network presets。
- 以 process tree 作为主要过滤维度。
- 支持 launch process 和 attach pid 两种 target。
- attach 不回填历史，只记录 `recording.start` 之后的事件。
- WPR/WPAExport 不作为主采集链路，只作为未来诊断、比对或 fallback 方向。

安装态 service 可以为 ETW recording 使用 LocalSystem 或 runtime config 指定的受控 identity。IDA reverse worker 使用 active interactive session 用户，不使用 LocalSystem fallback。开发态 `service run` 使用当前用户；权限不足时返回结构化错误，不自动提权。

## 校验原则

- 本地服务默认只绑定 loopback。
- 安装态服务配置和机器级 bearer token 默认属于 `%ProgramData%\DbgAtlas\etc\`。
- 安装态 bearer token 不进入 `runtime.toml`，CLI 普通输出不显示 token 内容。
- 对外 HTTP API（包括 `/rpc` 和 `/mcp`）需要 bearer token，并校验来自浏览器类客户端的 `Origin`。
- path、symbol path、proxy value 拒绝控制字符。
- proxy env 只允许明确支持的 key。
- recording provider/preset/filter 配置不得写入 workspace manifest；workspace 只保存实际 recording metadata 和 artifact 引用。
- 运行时配置可随机器变化；workspace artifacts 必须保持可审计和可复现。
