# M2 中心库验收

日期为 2026-11-21。验证对象是本机 Windows x64 安装服务和当前工作树的 MSVC release payload。

| 场景 | 验证结果 |
| --- | --- |
| 工程回归 | `cargo test --workspace` 共 252 项通过。覆盖两阶段注册重试、拒绝重绑定副本、运行期与历史状态、分页、UTF-8 输出、只读管理接口、录制与调试回归。 |
| 注册、离线与迁址 | 自动化测试使用独立的磁盘库。两个项目的同名工作区分配不同 UUID。新建 ServiceHost 并重新打开数据库后，生产者和使用方仍可查询。移动目录后，活动会话可关闭，归档错误单列。迁址须等待本次服务中的活动会话和未完成清理结束。 |
| Release 与更新 | 运行 `script/build-release-install.ps1 -BuildOnly`，通过已连接 MCP 的 `service.update(restart=true)` 更新。安装态健康检查返回 0.4.0 / ok，四个安装文件的 SHA-256 与 release payload 一致。 |
| 真实 MCP 调试 | alpha 和 beta 两个测试项目分别启动短生命周期的 cmd.exe，执行各自的 `.echo` 标记并关闭。按项目和执行 ID 查询无串项。以受理时刻为起点的 1 ms 时间区间可以查回原执行。 |
| 真实产物复用 | 在 alpha 录制 cmd.exe 得到 37,748,736 字节的 TTD .run。以登记的 Artifact ID 打开新回放会话；首次等待返回 running，通过原 Execution 等待到成功，执行 `r rip` 并正常关闭。中心库反查返回原录制会话和回放使用方。 |
| 安装服务重启 | 关闭测试会话后再次通过 MCP 更新并重启。核对服务实例已变更，通过 HTTP MCP 发现两个项目，查回 alpha 原命令的四条更新和完整输出。安装态 CLI 查询相同记录；活动会话为零。实际中心库恰有五张业务表，SQLite 完整性检查返回 ok。 |
| 项目临时离线 | 临时移开本轮 alpha 项目目录，MCP 仍能查询已知执行结果、历史和 TTD 的来源及使用方。读取缺失文件时收到明确错误。目录原位恢复后重新读到同一条输出。 |
| 管理页 | 全新 Chrome headless profile 直接从首页发现项目，选择 alpha，筛选真实命令、展开执行历史、查看产物来源与完整输出。时间筛选空结果正确，390 px 宽度无页面横向溢出，无 JavaScript 异常。 |

## 可核对的记录

- 中心库为 `%ProgramData%\DbgAtlas\data\history.sqlite3`。两个工作区、验收断言和客户端输出位于仓库的 `temp/m2-validation/`，包括 `ids.json`、`installed-checks.json`、`offline.log`、`browser-summary.json`、`payload-hashes.json` 和构建、测试日志。
- alpha 标记执行为 `op-b61411a4-40ff-40d8-9995-26c61d54fff0`。TTD 产物为 `artifact-73e2b0ec-cfc4-413b-b143-a7ecd04bbdb6`，录制与回放会话分别为 `recording-a3609ba3-e0ab-4e50-89d1-ed424401a39f` 和 `session-8f969c2c-174d-4465-b291-6587f3e513a9`。

## 验证范围

新建工作区使用 manifest 格式 2。旧格式通过保留原文件并提示使用新工作区来处理。本轮磁盘离线实验使用测试目录改名。断电、整卷物理脱离、磁盘满、数据库损坏恢复、索引重建及大型工作区性能仍待后续专项验收。
