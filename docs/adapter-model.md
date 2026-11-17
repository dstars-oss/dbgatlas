# 后端执行边界

本文区分目标后端契约与当前 `dbgatlas-adapter` 短调用抽象。环境/会话规格见[环境与会话模型](environment-session-model.md)，进程责任见[进程与生命周期](worker-model.md)。

## Backend、adapter 与 transport

- Backend：提供可操作的调试/采集上下文，负责原生执行和真实状态。
- Native adapter：封装特定本机能力，例如 DbgEng C ABI；属于后端实现细节。
- Transport：把请求、结果和事件送到后端，不决定调试命令含义。
- Worker：隔离和承载后端的进程方式；本轮所有活动会话绑定 DbgAtlas 专用 worker；后续接入既有后端仍需保持其独立所有权。

同一后端可有本机或远端连接方式。后端协议、控制端 RPC/MCP 和 native C ABI 是不同边界；不能直接把指针、本机句柄或控制端的绝对路径传成通用远端参数。

## 目标契约

| 职责 | 最小语义 |
| --- | --- |
| 发现与建立 | 环境、后端类型/能力、创建或接入、目标关联 |
| 原生执行 | 会话、操作身份、原生命令/脚本、执行上下文 |
| 结果 | 完成情况、后端错误、输出预览、完整输出引用 |
| 状态与事件 | 连接及目标状态、来源与未知信息；运行事件不依赖命令文本猜测 |
| 控制 | 查询、中断、关闭与支持的处置方式；明确进程所有权 |
| 输出访问 | 执行侧文件位置与逻辑输出引用分离，本机映射/后续远端取回 |

表中是职责而非要求公开等量 MCP 工具。保留后端原生文本、错误和扩展字段，避免将所有寄存器、断点、符号命令包装成平台私有语义。必要的结构化控制不能被正在执行的命令永久阻塞。

能力需说明“后端支持什么”和“当前会话实际可用什么”；不支持的操作明确报错。原生 exec 能修改目标和调试器配置，不能整体标记为只读，也不能只靠命令白名单推断执行后的现场状态。

## 实施阶段

迭代 2 实现环境/会话绑定、后端进程所有权、连接与执行责任分离，并让当前本机 debug、ETW 和 TTD 录制路径通过专用 worker 边界。允许最小必要的请求/响应调整，需同步 CLI、RPC/MCP、测试和文档，不强行保留本机 worker 字段作为平台概念。

迭代 3 采用 Session + Execution：session.exec 接受原生命令，execution.* 追踪执行与输出，session.get/read 观察现场和会话事件。等待超时只结束本次等待，不能隐含杀 worker；execution.cancel、session.interrupt 和 session.kill 分别表达取消执行、中断目标和强制清理。命名与接口契约见[执行交互方案](execution-interaction.md)；具体 MCP 版本、Tasks 映射及客户端兼容策略在实际验证后确定，不声称当前已支持。

迭代 6 实现远端 transport、连接身份、断线核对和文件回收。操作重试需先核对执行结果，不承诺跨崩溃的 exactly-once；结果无法确认时保留未知。迭代 2 的接口可表达远端，不等于已经完成远端验证。

## 当前实现及限制

`dbgatlas-adapter` 当前定义 `AdapterId`、`Capability`、`Invocation`、`InvocationResult` 和同步 `Adapter::invoke()`。`Invocation` 带可选本机 workspace/output `PathBuf`；结果包含 payload、产物及原始输出。`dbgatlas-core::Core` 查找 adapter、调用并登记记录。

此抽象没有完整的会话连接、进程所有权和异步中断契约；不能直接升级成覆盖所有后端的大接口。现有调试链路主要由 `ServiceHost`、`WorkerSupervisor` 与 worker 消息执行，不是全部经过 `Core::invoke()`。

现有 worker protocol v9 是内部本机 JSONL 协议，包含 `StartDebugSession`、`EvalDebugCommand` 等请求及本机 `artifact_dir`。它可以作为本地实现材料，但不能直接视为最终跨机器协议。

旧文档“第一版不设计 session trait/streaming callback”只反映早期短调用范围，不再限制当前环境、会话和进程架构调整。仍不引入与已实现路径无关的大型事件总线、插件系统或统一命令语言。

## 后端实现选择

自有 DbgEng/ETW native 能力继续通过 safe wrapper 隔离 FFI；GDB 等可使用原生结构化协议，其他后端可使用库或子进程。官方 DbgX MCP 是否替代某条现有路径，须验证工具 schema、状态、长命令、中断、关闭和客户端兼容性后决定。

不要求所有后端链接 C++ DLL，也不因采用 exec 就把后端降为没有状态与生命周期的终端。具体 crate 拆分以依赖和可验证职责为准。
