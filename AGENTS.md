# AGENTS.md

本文件定义 DbgAtlas 仓库根目录的工程协作规则和总体目录边界。更深层目录可以通过自己的 `AGENTS.md` 追加更具体的约束；若发生冲突，遵循离被修改文件最近的规则。

## 项目定位

DbgAtlas 的目标是打造面向 AI 的调试平台：让 AI 使用原生调试命令，可靠地操作本机或远端的调试后端，查询状态，并检索、引用可跨会话继承的操作历史与输出。先完善 Windows 调试链路，再逐步接入 Linux 等平台。

IDA 静态分析属于外部工具职责，仓库不提供 IDA adapter、SDK binding 或 reverse 接口，也不维护其历史兼容层和退役说明。调试所需的符号、类型、局部反汇编和地址信息仍在项目范围内；更高层的逆向知识是否产品化，随实际需求决定。

MCP、CLI 和后续管理页面都是入口层，不是架构核心。平台负责后端连接、会话、操作、状态和历史留存；调试后端保留原生语义，执行位置与控制端位置解耦。虚拟化管理后续独立设计。

根目录 `GOALS.md` 是当前迭代计划和进度的依据。计划描述目标形态，不代表能力已经实现；现有实现与旧架构文档随对应迭代逐步迁移，不做一次性整体重写。

## 总体目录结构

仓库的第一层目录按代码归属和工程职责划分：

```text
dbgatlas/
  AGENTS.md
  GOALS.md
  Cargo.toml
  CMakePresets.json
  README.md

  crates/                 # Rust crate：core、model、recording、CLI、service、safe wrapper、*-sys binding
  native/                 # 项目自有 C++：native adapter、C ABI header、CMake、内部 helper
  3rdpart/                # C++ 第三方依赖：vendored source、prebuilt、patch、license、CMake glue
  docs/                   # 架构、FFI、adapter、timeline/evidence、roadmap 等设计文档
  schemas/                # 面向持久化、AI 输入和工具交换的 JSON / JSONL schema
  examples/               # 示例 session、输入文件、命令和最小可复现工作流
  tests/                  # 跨 crate / native adapter / 进程边界的集成测试与 fixtures
  script/                 # 面向开发者的本机开发、构建和安装脚本
  xtask/                  # 构建、打包、binding 生成、schema 校验、发布整理等自动化
  temp/                   # 本机临时文件、一次性工具、测试 scratch；内容不提交
```

除非任务明确要求，不要引入新的顶层目录。新增顶层目录前应先确认它不能合理归入以上目录。

## 项目产物

本仓库产出 DbgAtlas 工具本身，不默认承载真实调试/逆向分析数据。真实分析数据应放在显式的 analysis workspace 中，由 CLI、MCP 或后续 UI 创建和打开。

analysis workspace 的框架先保持简单、可见、可扩展：

```text
analysis-workspace/
  artifacts/              # DbgAtlas 管理的工具产物、原始输出及相关记录
  analysis/               # 人/模型输出的 Markdown 笔记、假设、结论和报告
  inputs/                 # 可选：人工放入的初始输入材料
```

操作记录统一关联输入、执行上下文、状态与输出；不额外引入面向调用者的 Observation 对象。文件产物通过引用关联，异步事件可以归入会话记录，不强行假定一个输入只有一个输出。

`artifacts/` 是工具事实层，`analysis/` 是解释层，由人或模型用 Markdown 写作；高层语义、假设、结论和报告不应伪装成工具事实。历史按目标身份关联，允许跨会话检索，但不同会话的当前线程、回放位置等运行状态保持独立。

按 `GOALS.md` 引入每个 analysis workspace 的本地 SQLite，管理操作记录和输出索引；大产物保留为文件。区分不可丢失的原始记录与可重建的派生索引，不让多个节点直接共享写入同一个数据库文件。具体数据库路径、表结构和文件布局在对应迭代确定，不提前铺设。

## 开发语言

Rust/C++ 边界不通过中心化 `protocol` 模块定义。每个 native adapter 使用自己的 C header 和对应 Rust `*-sys` crate 局部定义 FFI 边界。

语言规范：

1. Rust 使用当前 stable toolchain，edition 以后续 `Cargo.toml` 为准。
2. C++ 使用 C++20 标准。
3. Windows native / C++ adapter 的开发、验证、release payload 和安装态服务更新必须使用 MSVC toolchain 构建；除非任务明确要求调查兼容性，不要使用 MinGW / UCRT 产物作为提交前构建或安装验证依据。
4. C++ 代码优先使用 RAII 管理内部资源，但不得把 C++ 类型暴露到 C ABI。
5. 对外 C ABI header 使用 C-compatible 类型和固定宽度整数类型。

跨语言边界规则：

1. 复杂 native 对象留在 C++ DLL 内部，Rust 只持有 handle 或 safe wrapper。
2. 简单固定结构使用 C struct 和 Rust `#[repr(C)]` mirror。
3. 数组使用 view + owner 模式；谁分配谁释放。
4. 字符串统一 UTF-8，复杂列表优先使用 string table + offset。
5. 大对象通过 artifact 文件传递，不通过 FFI 大块返回。
6. Rust panic 不允许进入 C callback 或 FFI 边界。

## 架构原则

1. Rust-first，而不是 Rust-only。
2. Rust 负责平台核心和编排；控制端、调试后端及目标的位置分别表达，本机与远端后端复用执行契约。
3. 自有 Windows native adapter 通过 C++ DLL 和 Rust safe wrapper 接入；其他后端可通过原生协议、库或子进程接入，不要求全部使用 C ABI。
4. `core` 层不得直接散落 unsafe FFI。
5. CLI、MCP 和后续 UI 必须调用 Rust core / wrapper 层，不得绕过 core 重复实现底层调试逻辑。
6. 项目命名、模块命名和目录结构不得绑定到 MCP、IDA、WinDbg、TTD 任一单点能力。
7. 在真实使用体验和需求边界明确前，不要过早设计完整 GUI、完整 DbgEng wrapper 或复杂序列化框架。
8. 持久化优先服务 AI 输入、审计和复现；具体格式随功能演进，优先保持可读、可审计、可迁移。
9. 面向 AI 优先提供原生命令执行与少量会话、状态、中断、历史接口，不将全部调试器命令重新包装成自有工具；内部状态应优先来自后端 API 或结构化事件。
10. 区分调用等待超时、操作取消、目标中断、目标终止及会话关闭；客户端或网络断开不能直接等同于目标退出。明确后端进程所有权及关闭策略。
11. 长操作优先复用经实际客户端验证的 MCP 标准能力，并保留必要的兼容路径；协议任务状态不能替代真实调试状态，持久化历史不能依赖协议任务的保留期限。
12. 跨平台采用小公共契约、能力声明和平台扩展；不假定所有后端具备相同功能，也不将 Windows 路径、PID 或 x64 寄存器模型固化为公共语义。

## 工程协作要求

修改前应先阅读相关目录的 `AGENTS.md`、相邻代码、测试和文档。改动应遵循最小必要变更原则，不做与任务无关的重构、格式化或目录迁移。

每个迭代聚焦 `GOALS.md` 中的 1—2 个目标，按验收场景推进。完成时同步更新进度、验证证据和未覆盖项；计划、静态检查、测试通过与安装态验证必须区分。纯文档调整执行 `git diff --check` 并检查语义一致性，无需构建或更新安装态服务。

## Git 分支协作

默认在当前本地分支上开发、提交和推送。不要仅因为当前分支是 `main`、`master` 或默认分支就自动创建 `codex/` 工作分支，也不要把普通“提交并推送”请求升级为 PR workflow。

只有在用户明确要求新分支、worktree、PR、隔离实验或指定分支策略时，才创建或切换分支。推送时优先推送当前分支到它的 upstream；若当前分支尚无 upstream，默认推送同名分支到 `origin`，除非用户另有指定。

新增模块时，先确认它属于 Rust、项目自有 C++、第三方依赖、文档、schema、示例、测试还是工程自动化，再放入对应目录。

临时文件、一次性测试输入/输出、生成的 scratch 数据、临时 IDA 数据库、临时 workspace 和只为当前任务服务的辅助工具，应默认放在仓库根目录的 `temp/` 下。除非外部工具强制要求使用系统临时目录，不要把开发或验证产物写入系统 `%TEMP%` / `/tmp`；若必须使用系统临时目录，应在完成后清理或在结果说明中标明。需要长期维护、可复用或应提交的脚本不要放在 `temp/`，应按职责放入 `script/` 或 `xtask/`。

`script/` 用于放置开发者可直接运行的本机脚本，例如 release 构建、安装、服务 lifecycle 辅助脚本。跨平台、可组合、面向 CI/发布流水线的工程自动化优先放入 `xtask/`。

完成 feature 后，除先运行与改动最相关的窄范围测试外，还应执行一次安装态端到端验证：

1. 使用维护脚本构建 release payload：从仓库根目录运行 `.\script\build-release-install.ps1 -BuildOnly`，并确保生成的 `target\release` payload 完整。
2. 通过 Codex 连接的 DbgAtlas MCP 调用 `service.update`，传入 `source_dir` 为当前仓库的绝对 `target\release` 路径，并使用 `restart: true`。不要用手动停止服务、复制文件或重新执行安装脚本替代这一步，除非当前机器还没有安装态服务；这种情况下应先说明前置条件缺失。
3. 服务重启后，通过 MCP 调用 `service.health` 确认安装态服务可用。
4. 针对刚完成的 feature，优先通过安装态服务暴露的 MCP 能力做一个最小冒烟验证；若该 feature 没有对应 MCP 入口，应说明已完成到 release 构建和安装态服务更新为止，以及剩余未覆盖的验证面。

完成非平凡代码、构建、schema 或测试改动后，应运行最相关且范围最小的验证命令。若无法运行，应说明原因和已完成的替代检查。

## 当前运行入口约定

`dbgatlas service` 是当前安装态和开发态的主入口。它在同一个 loopback HTTP listener 上暴露两个 endpoint：`/rpc` 是 DbgAtlas 自有 JSON-RPC API，`/mcp` 是 Codex 等 MCP 客户端使用的 HTTP MCP endpoint。项目不再维护独立的 stdio MCP server 或 `dbgatlas-mcp` crate。

`/rpc` 和 `/mcp` 不使用认证，信任本机调用者；二者共享 loopback bind 限制和浏览器类客户端的 `Origin` 校验。这些限制不提供本机用户或进程隔离。Codex 项目配置可放在本地 `.codex/config.toml`，但 `.codex/` 应保持 ignored，不提交。

上述约定描述当前入口。未来远端后端通信在对应迭代单独设计，不通过直接放宽现有 listener 的 loopback 限制来替代远程连接设计。
