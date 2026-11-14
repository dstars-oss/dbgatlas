# 会话与操作模型

本文定义迭代 2 的业务契约。实现进度和运行证据分别见 [PLAN.md](../PLAN.md)、[当前实现基线](roadmap.md)与[迭代 2 验证记录](iteration-2-validation.md)。进程处置见[进程与生命周期](worker-model.md)。

## 核心资源

Session 和 Operation 是两个核心业务资源。环境、目标、后端、执行身份和处置策略是会话的结构化信息；产物复用已有引用。

会话是在一个工作区中，由专用工作进程承载、能够持续执行操作的一段执行生命周期。一个活动会话固定绑定一个工作进程实例，一个 worker 只承载一个会话。活动期间不替换、复用、透明重启或迁移 worker。创建阶段可以尝试候选进程，但只有确认失败副作用已清理且重试安全时才能继续。worker 退出后重新打开必须创建新会话。

`session_id`、worker 身份、worker 进程实例和目标进程实例分别标识。客户端连接不是会话；客户端断开不关闭会话。目标退出也不要求 worker 立即退出。多个会话可以打开同一离线文件，当前线程、栈帧和回放位置各自独立。

## 公共摘要与内部运行对象

`dbgatlas-model::Session` 保存可序列化事实；`ManagedSession`、`RemoteRecording`、进程 supervisor 保存锁、Child、管道等本机运行对象。native handle 和 Windows token 不进入公共模型。

| 信息 | 内容 |
| --- | --- |
| 基本信息 | session_id、debug/recording 类型、workspace、时间、service_instance |
| 环境 | control_environment、worker.environment、在线 target.environment |
| worker | worker_id、process、backend、execution_identity、ownership |
| 目标 | revision、kind、open_request、process、location、created_by_session |
| 现场 | context；后端可提供的线程、栈帧或录制状态，未知值为空 |
| 控制 | capabilities、正常/异常处置 policy |
| 诊断 | lifecycle、connection、target_state、cleanup、error、warnings |

本机默认环境别名为 `local`，不需要环境创建接口。环境身份、系统启动实例、服务运行实例分别表达；当前无法确认系统启动实例，`system_instance` 为 null。该别名不构成跨机器或 VM 快照身份。

进程实例包含 environment_id、PID 和可取得的创建时间。FILETIME 在 JSON 中使用十进制字符串，兼容读取旧数字，避免 JavaScript 精度损失。本机以 Windows FILETIME 创建时间及执行侧进程句柄核对，不能按裸 PID 终止。路径包含环境归属；只有对应执行侧可解释其为本地路径。离线输入文件位于 worker 环境，其来源主机不代表当前在线目标环境。

## 请求与绑定

- launch 保存程序及参数，成功后填入实际进程实例；发出 launch 请求不等于已确认目标身份。
- attach 由执行侧核对进程实例，不获得目标所有权。
- open_file 保存环境内文件位置，不因路径相同而声称内容相同；稳定内容身份后置。
- TTD monitor 的程序和命令行筛选条件保留为 selector，不伪装成已绑定的具体进程。
- 后端观察到绑定变化时递增 revision；旧操作的上下文快照不随之变化。本轮不增加主动切换目标接口。

## 状态维度

| 维度 | 值 |
| --- | --- |
| 会话生命周期 | opening / active / closing / closed / failed |
| worker 连接 | connected / disconnected / unknown |
| 在线目标 | unknown / running / stopped / partially_stopped / exited |
| 离线目标 | offline，回放位置独立表达 |
| 操作执行 | pending / running / success / failed / canceled / unknown |

状态来自引擎 API、结构化结果或进程监测，附带观察来源和时间，不解析命令名猜测。失联时当前状态为未知，last_known 保留上次已知值。摘要的观察时间不是实时性的保证。`failed` 不代表清理完成，`closed` 不代表目标退出；cleanup.complete 与错误列表独立。

当前 DbgEng 回报引擎运行状态、系统 PID、线程索引和可用的栈帧索引。NO_DEBUGGEE 也可能由 detach 引起，因此不能直接解释为目标已退出。完整事件消费、回放位置和可靠目标中断在迭代 3 深化。

## 操作与记录

每个业务动作分配操作 ID，保存会话、动作、原始命令或结构化输入、实际执行时的 worker/目标/现场快照、状态、时间和零到多个输出。打开、命令、现场读取、取消请求、关闭均记账。读取已有摘要、轮询状态和读取历史文件默认不再生成业务操作。

已接受操作先记录再执行。打开操作先记 pending，候选 worker 开始执行时补充上下文并记 running。批量命令保存 batch 和每步操作，步骤输入包含 batch ID。取消请求有独立操作并关联原操作；收到请求不等于完成取消。worker 失联且结果不确定时记 unknown，不自动重放。

同会话普通操作串行，忙时明确拒绝（RPC -32017），不提供排队系统。摘要查询和强制停止不等待普通请求锁。后端不支持的控制能力明确报错；当前 TTD 同步录制的普通 stop 不具备独立中断能力，使用明确的 force-close 或既有录制时长策略。

执行结果和输出可用性分别记录：已知命令完成后发生 native 输出写入、产物登记或结果记账失败，保留已知执行状态和 output_error，不能诱导重复执行命令。

## 接口与兼容

- 保留 debug.*、recording.*、operation.*；新增 session.get。
- session.list 默认返回当前 debug 范围；kind 支持 debug/recording/all，include_terminal 可包含本服务运行期的终态摘要。
- 新录制 session_id 与 recording_id 使用同一个标识值，后者继续作为兼容字段。
- 业务响应增加 session 公共摘要。旧字段由适配层保留，不能将旧 Ready/Error 等字段当作完整目标状态。
- CLI、RPC、MCP 复用 ServiceHost；不增加任意 JSON 的 operation.create。
- 内部 worker 协议为 v7，拒绝不匹配版本；它仍包含本机路径，不是远端通信协议。

原始更新追加到既有 JSONL，同一操作使用同一 ID 和递增 update_seq。读取合并为最新状态，原始行保留；旧字段缺失按未知处理，不造身份或成功结果。会话元数据存为既有 session/recording 目录内的版本化 session.json。服务重启不恢复 worker，也不把历史 active 当作当前会话；完整跨重启查询在迭代 4 实现。

本轮不实现 SQLite、远端传输、VM 管理、稳定内容匹配、worker 池、自动恢复或完整多进程调试。现有 debug 执行超时可能终止 worker，是兼容行为，不是新的调用等待预算语义。
