# AGENTS.md

本文件定义 DbgAtlas 仓库根目录的工程协作规则和总体目录边界。更深层目录可以通过自己的 `AGENTS.md` 追加更具体的约束；若发生冲突，遵循离被修改文件最近的规则。

## 项目定位

DbgAtlas 是一个面向 Windows 的调试、逆向、事件录制与问题分析平台。项目以工具会话、原始输出、artifact 和可复现操作为基础，串联 WinDbg / CDB / DbgEng、IDA、Dump、ETW、TTD、Symbol / PDB / DIA / DbgHelp 等能力。

MCP、CLI 和后续 UI 都是入口层，不是架构核心。底层 Windows native 能力通过 C++ adapter 暴露为 C ABI，由 Rust 侧安全封装后进入平台核心。

## 总体目录结构

仓库的第一层目录按代码归属和工程职责划分：

```text
dbgatlas/
  AGENTS.md
  GOALS.md                  # milestone task list：完成 feature 后同步更新
  Cargo.toml
  CMakePresets.json
  README.md

  crates/                 # Rust crate：core、model、record、CLI、MCP、safe wrapper、*-sys binding
  native/                 # 项目自有 C++：native adapter、C ABI header、CMake、内部 helper
  3rdpart/                # C++ 第三方依赖：vendored source、prebuilt、patch、license、CMake glue
  docs/                   # 架构、FFI、adapter、timeline/evidence、roadmap 等设计文档
  schemas/                # 面向持久化、AI 输入和工具交换的 JSON / JSONL schema
  examples/               # 示例 session、输入文件、命令和最小可复现工作流
  tests/                  # 跨 crate / native adapter / 进程边界的集成测试与 fixtures
  script/                 # 面向开发者的本机开发、构建和安装脚本
  xtask/                  # 构建、打包、binding 生成、schema 校验、发布整理等自动化
```

除非任务明确要求，不要引入新的顶层目录。新增顶层目录前应先确认它不能合理归入以上目录。

## 项目产物

本仓库产出 DbgAtlas 工具本身，不默认承载真实调试/逆向分析数据。真实分析数据应放在显式的 analysis workspace 中，由 CLI、MCP 或后续 UI 创建和打开。

analysis workspace 的框架先保持简单、可见、可扩展：

```text
analysis-workspace/
  artifacts/              # DbgAtlas 管理的工具产物、原始输出、元数据和可重建索引
  analysis/               # 人/模型输出的 Markdown 笔记、假设、结论和报告
  inputs/                 # 可选：人工放入的初始输入材料
```

`artifacts/` 是工具事实层，记录和组织 DbgAtlas 实际接触到的 target、session、工具输入输出、文件材料和低层可机械提取信息。`analysis/` 是解释层，由人或模型用 Markdown 写作；高层语义、假设、结论和报告不应伪装成 DbgAtlas 自己产生的客观 JSONL 事实。

workspace 内部是否需要具体的 JSONL 文件、索引目录或 artifact 子目录，应随功能逐步引入。不要在尚未实现相关能力前预设过细的持久化结构。

## 开发语言

Rust/C++ 边界不通过中心化 `protocol` 模块定义。每个 native adapter 使用自己的 C header 和对应 Rust `*-sys` crate 局部定义 FFI 边界。

语言规范：

1. Rust 使用当前 stable toolchain，edition 以后续 `Cargo.toml` 为准。
2. C++ 使用 C++20 标准。
3. C++ 代码优先使用 RAII 管理内部资源，但不得把 C++ 类型暴露到 C ABI。
4. 对外 C ABI header 使用 C-compatible 类型和固定宽度整数类型。

跨语言边界规则：

1. 复杂 native 对象留在 C++ DLL 内部，Rust 只持有 handle 或 safe wrapper。
2. 简单固定结构使用 C struct 和 Rust `#[repr(C)]` mirror。
3. 数组使用 view + owner 模式；谁分配谁释放。
4. 字符串统一 UTF-8，复杂列表优先使用 string table + offset。
5. 大对象通过 artifact 文件传递，不通过 FFI 大块返回。
6. Rust panic 不允许进入 C callback 或 FFI 边界。

## 架构原则

1. Rust-first，而不是 Rust-only。
2. Rust 主进程负责平台核心和编排。
3. C++ DLL 负责 Windows native 能力，尤其是 DbgEng、DIA、ETW、DbgHelp、MiniDump 等。
4. `core` 层不得直接散落 unsafe FFI。
5. CLI、MCP 和后续 UI 必须调用 Rust core / wrapper 层，不得绕过 core 重复实现底层调试逻辑。
6. 项目命名、模块命名和目录结构不得绑定到 MCP、IDA、WinDbg、TTD 任一单点能力。
7. MVP 阶段不要过早设计完整 GUI、完整 DbgEng wrapper 或复杂序列化框架。
8. 持久化优先服务 AI 输入、审计和复现；具体格式随功能演进，初期可优先考虑 JSONL。

## 工程协作要求

修改前应先阅读相关目录的 `AGENTS.md`、相邻代码、测试和文档。改动应遵循最小必要变更原则，不做与任务无关的重构、格式化或目录迁移。

新增模块时，先确认它属于 Rust、项目自有 C++、第三方依赖、文档、schema、示例、测试还是工程自动化，再放入对应目录。

`script/` 用于放置开发者可直接运行的本机脚本，例如 release 构建、安装、服务 lifecycle 辅助脚本。跨平台、可组合、面向 CI/发布流水线的工程自动化优先放入 `xtask/`。

`GOALS.md` 是项目的 milestone task list。完成一个 feature 后，应在同一个变更中更新对应 checkbox；只有实现、文档和验证都完成后才标记为已完成。如果 feature 改变了里程碑范围，应先同步调整 `GOALS.md`，再继续实现。

完成非平凡代码、构建、schema 或测试改动后，应运行最相关且范围最小的验证命令。若无法运行，应说明原因和已完成的替代检查。

## 当前运行入口约定

`dbgatlas service` 是当前安装态和开发态的主入口。它在同一个 loopback HTTP listener 上暴露两个 endpoint：`/rpc` 是 DbgAtlas 自有 JSON-RPC API，`/mcp` 是 Codex 等 MCP 客户端使用的 HTTP MCP endpoint。项目不再维护独立的 stdio MCP server 或 `dbgatlas-mcp` crate。

`/rpc` 和 `/mcp` 共享 bearer token、loopback bind 限制和浏览器类客户端的 `Origin` 校验。Codex 项目配置可放在本地 `.codex/config.toml`，但 `.codex/` 应保持 ignored，不提交；token 通过 `DBGATLAS_TOKEN` 等本机环境变量传入，不写进仓库。

`dbgatlas service install` 不应覆盖已有 `%ProgramData%\DbgAtlas\etc\runtime.toml` 或 `%ProgramData%\DbgAtlas\etc\token`。`--force` 只用于更新 installed payload 和 Windows service entry，不重置 token/config。

涉及 service、MCP、CLI HTTP client 或安装态行为的改动，优先运行：

```powershell
cargo test -p dbgatlas-service
cargo test -p dbgatlas-cli
cargo test --workspace
```

验证安装态 service / MCP 时，优先检查 `dbgatlas service status --json`、`%ProgramData%\DbgAtlas\var\log\service-YYYY-MM-DD.log`，以及相关 analysis workspace 的 `artifacts/operations.jsonl` 和 `artifacts/command_audit.jsonl`。
