# Runtime Model

DbgAtlas 区分 analysis workspace 和 runtime install/config。workspace 保存项目事实；runtime config 保存执行环境的工具、进程和网络策略。本文件安装路径与参数描述当前 Windows 本机实现；环境归属与目标规格见 [环境与会话模型](environment-session-model.md)。

## 迭代 2 的环境边界（目标规格）

- 环境是执行/目标定位上下文，本机提供默认环境；不等同于虚拟化管理，也不要求每个环境都安装 DbgAtlas 服务。
- 后端环境解析其工具、PID、工作目录和本地文件，控制端保存关联与逻辑输出引用。远端环境的路径不能通过控制端 PathBuf 直接验证或打开。
- 后端进程所在环境与目标环境可以不同，例如本机调试器连接远程内核；本机/远端连接不决定目标语义。
- 本机 LocalSystem、active interactive user、SCM、named pipe 和安装 runtime 路径属于 Windows 执行实现，不作为所有平台的环境协议。
- 迭代 2 建立归属和接口边界，迭代 6 才实现远端连接/部署；不提前新增远端地址、凭据或 VM 快照配置。

## 当前本机实现

## Analysis Workspace

analysis workspace 是显式目录，包含：

```text
analysis-workspace/
  dbgatlas-workspace.json
  artifacts/
  analysis/
  inputs/
```

`dbgatlas-workspace.json` 只记录 workspace identity、schema version 和创建信息，不记录本机 DbgEng、ETW、proxy 或 service install root。

对外 service API 不暴露 workspace 资源。调用方在创建 session 时传入 `project_root`，service 内部固定使用 `<project_root>/dbgatlas` 作为 analysis workspace；该目录是可见目录，不使用隐藏 `.dbgatlas`。

## Runtime Config

`dbgatlas-runtime` 定义类型、解析入口和校验。当前配置项包括：

- `tools.symbol_path`
- `process.child_identity`
- `process.fallback_child_identity`
- `process.elevate_if_admin`
- `proxy`
- `server.bind`
- `server.management_bind`（默认 `127.0.0.1:7332`）与 `server.management_enabled`（默认 true）；[管理页面](management-ui.md)使用独立 loopback listener。

安装态 Windows service 默认使用 machine-wide Program Files 下的 install root；安装器或提权脚本应复用 service 命令默认行为，只有自定义目录时才传入 `--install-root`：

```text
%ProgramFiles%\DbgAtlas\
  bin\                  # installed runtime payload
    dbgatlas.exe
    dbgatlas-worker.exe
    dbgatlas_dbgeng.dll
    dbgatlas_etw.dll
    rt\
      windbg\
        amd64\          # copied Store WinDbg runtime: dbgeng + ttd
  etc\
    runtime.toml        # runtime config
  var\
    log\
      service-YYYY-MM-DD.log
```

SCM 注册的 `DbgAtlas` service 指向 `<install-root>\bin\dbgatlas.exe`，不指向开发目录下的 `target\debug` 或 `target\release`。`dbgatlas service install` 支持 `--payload-mode copy` 和 `--payload-mode use-existing`：前者从 `--payload-dir` 复制 payload 到 `<install-root>\bin`，后者只验证 `<install-root>\bin` 中已有完整 payload，不覆盖文件。安装时发现完整 Store WinDbg runtime 会复制到 `bin\rt\windbg\<arch>`，使 TTD recorder/replay 从普通 Win32 路径加载匹配的 `dbgeng` 和 `ttd` 组件；若发现旧 `%ProgramData%\DbgAtlas\etc\` 或旧根目录下的 `runtime.toml`，新 `etc\` 缺失对应文件时会复制迁移。`dbgatlas service uninstall` 默认只移除 SCM entry 并保留 install root；`--purge` 才删除整个 install root。

安装态 service 还暴露 `service.update` JSON-RPC/MCP 方法，接收一个已经构建好的 payload 目录。该方法只完成校验并启动独立 updater 进程，然后异步返回 accepted；updater 会先复制到 `bin.next-*`，并在停服务前把 Store 或现有 DbgAtlas WinDbg runtime 放入 `bin.next-*\rt\windbg\<arch>`，停止服务后使用 rename 将旧 `bin` 移到 `bin.old-*` 并把新 payload 放到 `bin`，随后刷新当前版本的 SCM 启动命令，保留服务账户、启动类型和依赖，最后按请求重启服务并 best-effort 清理旧目录。payload 根目录的 `runtime.toml` 或 `etc/runtime.toml` 会在校验通过后覆盖安装态 `etc/runtime.toml`。更新结果写入 service 日志。

`service.update` 排障优先看 `<install-root>\var\log\service-YYYY-MM-DD.log`：`accepted service.update` 表示请求已通过校验并启动 updater；`starting service apply-update` 表示子进程开始 staged 替换；残留 `bin.next-*` 多半表示替换前中断，残留 `bin.old-*` 表示新 payload 已经接管但清理失败或被占用。日志确认后再调用 `dbgatlas --json service health` 检查重启后的 HTTP/RPC/MCP endpoint。


安装态 service 日志写入 `<install-root>\var\log\service-YYYY-MM-DD.log`，按 UTC 日期滚动，保留当天和前 6 天的 service 日志。

开发态 `dbgatlas service run --bind ...` 仍直接使用当前进程和当前目录，不注册 SCM，也不写入 install root。同一个 HTTP listener 暴露 `/rpc` 和 `/mcp`；二者均无需认证，信任本机调用者，并保留 loopback bind 与浏览器 `Origin` 校验。

DbgEng / TTD 路径解析不读取 runtime config 或环境变量中的本地安装路径。安装态 debug worker 默认使用 LocalSystem，由 service 启动参数中的 install root 显式解析 DbgAtlas 受控 runtime，并按当前机器状态自动发现候选，避免把 Store WinDbg 的版本化 WindowsApps 路径写死到 `runtime.toml`。当安装/更新阶段成功复制 Store WinDbg runtime 后，运行时会优先使用 `<install-root>\bin\rt\windbg\<arch>`，因此相关 DbgEng 候选和 TTD recorder 目录会一起切换到 DbgAtlas 受控目录。当 `.run` replay、用户目录 dump 或 live launch 需要交互用户权限时，调用方可在 `debug.session.create` 传入 `worker_identity: "active_interactive_user"`；如果当前交互用户是 UAC split admin，active interactive worker 会优先使用 linked elevated token。TTD `.run` replay 的 target 仍使用 `{ "kind": "file", "path": "trace.run" }`。

- DbgEng：DbgAtlas copied WinDbg runtime -> Store WinDbg -> Windows Kits / WDK Debuggers -> System32。
- TTD：按 DbgEng 候选顺序查找 `<dbgeng_dir>\ttd` -> Store TimeTravelDebugging。

debug worker 会按解析顺序接收 DbgEng 候选目录，并在 `LoadLibrary` 失败时尝试下一个候选。打开 `.run` 时，每个候选会进入独立 worker 尝试，因此 Store 版不支持或不可加载时可以降级到 SDK，再降级到 System32；`dbgeng.dll` 一旦在某个进程内成功加载，就不在同一进程内切换版本。

`dbgatlas service install` 创建新的 `runtime.toml` 时不写入自动发现的 DbgEng / TTD 路径。`service.update` 可用 payload 中的 `runtime.toml` 覆盖已有 config；payload `runtime.toml` 不得包含已移除的 `tools.dbgeng_dir` / `tools.ttd_dir` 字段。升级前已有 config 中的这些旧字段会被运行时忽略。

## Recording Runtime

recording runtime config 只保存本机工具和进程策略，不进入 analysis workspace manifest。ETW 由专用 recording worker 内的 NativeEtwController 调用 C++ ETW adapter；adapter 负责 ETW session、provider enable、实时消费、过滤和 flush。控制服务负责准入、公共会话和最终产物登记。

recording policy 的文档目标：

- 默认使用内置 process/thread/image/file/registry/network presets；CPU sampling 通过显式 `cpu` preset opt-in，默认不启用。
- 以 process tree 作为主要过滤维度。
- 支持 launch process 和 attach pid 两种 target。
- attach 不回填历史，只记录 `recording.start` 之后的事件。
- WPR/WPAExport 不作为主采集链路，只作为未来诊断、比对或 fallback 方向。

ETW worker 保持 service 的执行身份语义，当前不提供采集 worker 的请求级身份切换；采集目标的启动身份不能替代采集器身份。安装态 debug worker 默认使用 LocalSystem，并支持请求级 active interactive user override。`recording.ttd` 的专用 worker 默认沿用 service 身份，通过 helper 管理 `TTD.exe`；请求可用 `worker_identity: "active_interactive_user"` 将 recorder 和 timeout stop 命令切到当前交互用户会话，用于用户级 attach 到已运行 PID。helper 的 Job 在正常完成时解除终止约束，异常时清理其自建进程并保留失败退出码。开发态 `service run` 使用当前用户；权限不足时返回实际错误或已记录的降级信息，不声称自动获得采集权限。

## 校验原则

- 本地服务默认只绑定 loopback。
- 安装态服务配置默认属于 `<install-root>\etc\`。
- 对外 HTTP API（包括 `/rpc` 和 `/mcp`）不使用认证，并校验来自浏览器类客户端的 `Origin`。安装态服务及默认 debug worker 使用 LocalSystem，本机调用者可以访问包括调试和服务更新在内的能力；loopback 与 `Origin` 校验不提供本机用户隔离。
- path、symbol path、proxy value 拒绝控制字符。
- proxy env 只允许明确支持的 key。
- recording provider/preset/filter 配置不得写入 workspace manifest；workspace 只保存实际 recording metadata 和 artifact 引用。
- 运行时配置可随机器变化；workspace artifacts 必须保持可审计和可复现。
