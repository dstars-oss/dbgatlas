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

- `tools.symbol_path`
- `tools.etw.adapter_dir` 或等价 native ETW adapter 位置
- `tools.etw.default_presets`
- `tools.ida.install_dir`
- `tools.ida.python_executable`
- `tools.ida.vendor_src_dir`（历史兼容配置；IDA native adapter 现在使用仓库内固定的 IDA SDK header 快照构建，运行时打开 reverse session 不依赖该配置路径）
- `tools.ida.allow_py_eval`（默认 `false`；显式开启高权限 `reverse.py_eval` / IDAPython 执行能力）
- `process.child_identity`
- `process.fallback_child_identity`
- `process.elevate_if_admin`
- `proxy`
- `server.bind`

安装态 Windows service 默认使用当前用户 Local Programs 下的显式 install root；安装器或提权脚本应把该路径传给 `dbgatlas service install --install-root`，避免 LocalSystem / elevated 进程重新推导 `%LOCALAPPDATA%`：

```text
%LOCALAPPDATA%\Programs\dbgatlas\
  bin\                  # installed runtime payload
    dbgatlas.exe
    dbgatlas-worker.exe
    dbgatlas_dbgeng.dll
    dbgatlas_etw.dll
    dbgatlas_ida.dll
    rt\
      windbg\
        amd64\          # copied Store WinDbg runtime: dbgeng + ttd
  etc\
    runtime.toml        # runtime config
    token               # machine-local bearer token
  var\
    log\
      service-YYYY-MM-DD.log
```

SCM 注册的 `DbgAtlas` service 指向 `<install-root>\bin\dbgatlas.exe`，不指向开发目录下的 `target\debug` 或 `target\release`。`dbgatlas service install` 支持 `--payload-mode copy` 和 `--payload-mode use-existing`：前者从 `--payload-dir` 复制 payload 到 `<install-root>\bin`，后者只验证 `<install-root>\bin` 中已有完整 payload，不覆盖文件。安装时发现完整 Store WinDbg runtime 会复制到 `bin\rt\windbg\<arch>`，使 TTD recorder/replay 从普通 Win32 路径加载匹配的 `dbgeng` 和 `ttd` 组件；若发现旧 `%ProgramData%\DbgAtlas\etc\` 或旧根目录下的 `runtime.toml` / `token`，新 `etc\` 缺失对应文件时会复制迁移。`dbgatlas service uninstall` 默认只移除 SCM entry 并保留 install root；`--purge` 才删除整个 install root。

安装态 service 还暴露 `service.update` JSON-RPC/MCP 方法，接收一个已经构建好的 payload 目录。该方法只完成校验并启动独立 updater 进程，然后异步返回 accepted；updater 会先复制到 `bin.next-*`，并在停服务前把 Store 或现有 DbgAtlas WinDbg runtime 放入 `bin.next-*\rt\windbg\<arch>`，停止服务后使用 rename 将旧 `bin` 移到 `bin.old-*` 并把新 payload 放到 `bin`，最后按请求重启服务并 best-effort 清理旧目录。payload 根目录的 `runtime.toml` 或 `etc/runtime.toml` 会在校验通过后覆盖安装态 `etc/runtime.toml`；payload 中的 `token` 不会复制，安装态 token 始终保留。payload `runtime.toml` 本身也不得包含 `token` / `token_file` / `bearer_token` 等 token 字段。更新结果写入 service 日志。

`service.update` 排障优先看 `<install-root>\var\log\service-YYYY-MM-DD.log`：`accepted service.update` 表示请求已通过校验并启动 updater；`starting service apply-update` 表示子进程开始 staged 替换；残留 `bin.next-*` 多半表示替换前中断，残留 `bin.old-*` 表示新 payload 已经接管但清理失败或被占用。日志确认后再调用 `dbgatlas --json service health` 检查重启后的 HTTP/RPC/MCP endpoint。

安装态 service 日志写入 `<install-root>\var\log\service-YYYY-MM-DD.log`，按 UTC 日期滚动，保留当天和前 6 天的 service 日志。

开发态 `dbgatlas service run --bind ... --token ...` 仍直接使用当前进程和当前目录，不注册 SCM，也不写入 install root。同一个 HTTP listener 暴露 `/rpc` 和 `/mcp`；二者复用同一个 bearer token。开发态如需暴露高权限 IDAPython 执行能力，必须显式传入 `--allow-ida-py-eval`。

DbgEng / TTD 路径解析不读取 runtime config 或环境变量中的本地安装路径。安装态 debug worker 默认使用 LocalSystem，由 service 启动参数中的 install root 显式解析 DbgAtlas 受控 runtime，并按当前机器状态自动发现候选，避免把 Store WinDbg 的版本化 WindowsApps 路径写死到 `runtime.toml`。当安装/更新阶段成功复制 Store WinDbg runtime 后，运行时会优先使用 `<install-root>\bin\rt\windbg\<arch>`，因此相关 DbgEng 候选和 TTD recorder 目录会一起切换到 DbgAtlas 受控目录。当 `.run` replay、用户目录 dump 或 live launch 需要交互用户权限时，调用方可在 `debug.session.create` 传入 `worker_identity: "active_interactive_user"`；如果当前交互用户是 UAC split admin，active interactive worker 会优先使用 linked elevated token。TTD `.run` replay 的 target 仍使用 `{ "kind": "file", "path": "trace.run" }`。

- DbgEng：DbgAtlas copied WinDbg runtime -> Store WinDbg -> Windows Kits / WDK Debuggers -> System32。
- TTD：按 DbgEng 候选顺序查找 `<dbgeng_dir>\ttd` -> Store TimeTravelDebugging。

debug worker 会按解析顺序接收 DbgEng 候选目录，并在 `LoadLibrary` 失败时尝试下一个候选。打开 `.run` 时，每个候选会进入独立 worker 尝试，因此 Store 版不支持或不可加载时可以降级到 SDK，再降级到 System32；`dbgeng.dll` 一旦在某个进程内成功加载，就不在同一进程内切换版本。

`dbgatlas service install` 创建新的 `runtime.toml` 时不写入自动发现的 DbgEng / TTD 路径。`service.update` 可用 payload 中的 `runtime.toml` 覆盖已有 config，但不会覆盖安装态 token；payload `runtime.toml` 不得包含已移除的 `tools.dbgeng_dir` / `tools.ttd_dir` 字段。升级前已有 config 中的这些旧字段会被运行时忽略。

## Recording Runtime

recording runtime config 只保存本机工具和进程策略，不进入 analysis workspace manifest。ETW recording 默认通过受控 worker 调用 C++ ETW adapter，adapter 负责 ETW session、provider enable、实时消费、过滤和 flush。

recording policy 的文档目标：

- 默认使用内置 process/thread/image/file/registry/network presets。
- 以 process tree 作为主要过滤维度。
- 支持 launch process 和 attach pid 两种 target。
- attach 不回填历史，只记录 `recording.start` 之后的事件。
- WPR/WPAExport 不作为主采集链路，只作为未来诊断、比对或 fallback 方向。

安装态 service 可以为 ETW recording 使用 LocalSystem 或 runtime config 指定的受控 identity。安装态 debug worker 默认使用 LocalSystem，并支持请求级 active interactive user override；`recording.ttd` 默认在 service 进程身份下启动 `TTD.exe`，但请求可用 `worker_identity: "active_interactive_user"` 将 TTD recorder 和 timeout stop 命令切到当前交互用户会话，用于用户级 attach 到已运行 PID。IDA reverse worker 使用 active interactive session 用户，不使用 LocalSystem fallback。开发态 `service run` 使用当前用户；权限不足时返回结构化错误，不自动提权。

## 校验原则

- 本地服务默认只绑定 loopback。
- 安装态服务配置和机器级 bearer token 默认属于 `<install-root>\etc\`。
- 安装态 bearer token 不进入 `runtime.toml`，CLI 普通输出不显示 token 内容。
- 对外 HTTP API（包括 `/rpc` 和 `/mcp`）需要 bearer token，并校验来自浏览器类客户端的 `Origin`。
- path、symbol path、proxy value 拒绝控制字符。
- proxy env 只允许明确支持的 key。
- recording provider/preset/filter 配置不得写入 workspace manifest；workspace 只保存实际 recording metadata 和 artifact 引用。
- 运行时配置可随机器变化；workspace artifacts 必须保持可审计和可复现。
