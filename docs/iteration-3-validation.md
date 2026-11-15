# 迭代 3：验收范围

日期：2026-11-15。Session + Execution、有限等待、输出读取和 MCP/CLI 收敛均已实现并完成本机验证。安装态前置条件缺失，整个迭代尚未完成安装态验收。

| 阶段 | 结果 | 证据 |
| --- | --- | --- |
| 3A 机制与客户端原型 | DbgEng 执行/中断、Dump/TTD、官方 WinDbg MCP 对照与实际 Codex 握手；选择普通工具调用及 Execution 查询 | [3A 记录](iteration-3a-validation.md) |
| 3B 执行侧 | 独立控制管道、引擎事件与 epoch、持续事件等待；本机真实后端与 release 通过；提交 1430e61 | [3B 记录](iteration-3b-validation.md) |
| 3C 公共契约 | Session + Execution、短等待、游标读取、去重、关闭协调、来源与显式 Artifact 输入；提交 16013df | [3C 记录](iteration-3c-validation.md) |
| 3D 入口收敛 | 三版本 MCP、结构化结果、25 项工具列表、CLI；245 项工作区测试、MSVC release、实际 Codex app-server 调用、Dump/TTD 执行回归通过 | [3D 记录](iteration-3d-validation.md) |
| 安装态 | 未执行；本机无 DbgAtlas SCM 服务与已连接 DbgAtlas MCP 工具 | 待具备前置条件后通过 MCP service.update(restart=true)、service.health 和功能冒烟验收 |

原始输出 Artifact 保持来源 Session/Execution；异步会话输出仅绑定有证据的 Session。后续会话可以显式复用同一材料，关闭不会删除它；跨会话自动匹配、历史恢复和汇聚仍不属于本轮。

实际客户端验证覆盖传输和工具结果，不包含模型 turn、并发调度、通知消费或所有客户端 UI。ETW CPU/TTD 重新录制的本轮真实回归未覆盖；不把契约测试或此前迭代记录等同本轮运行。Tasks、请求级提前取消等待、SSE、远端及重启恢复均未声明为当前能力。具体限制、指标口径及复现命令以各阶段验证记录为准；计划状态由 [PLAN.md](../PLAN.md) 维护。
