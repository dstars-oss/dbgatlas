# 本机管理页面

管理页从服务中心库列出已登记项目及调试历史，并按产物引用读取项目文件。存储职责见[工作区模型](workspace-model.md)。

## 访问与配置

运行或更新 DbgAtlas 服务后，打开 **http://localhost:7332/**。RPC/MCP 继续使用 `127.0.0.1:7331`；管理端是同一服务进程中的独立 loopback listener，随服务启动和停止，不启动额外 Web 进程。HTML/CSS/JavaScript 内嵌在可执行文件中，不依赖 Node.js 或 CDN。

开发态：

```powershell
dbgatlas service run --bind 127.0.0.1:7331 --management-bind 127.0.0.1:7332
# 多个独立测试服务可关闭管理 listener，避免固定端口冲突
dbgatlas service run --bind 127.0.0.1:7441 --no-management
```

安装态使用 `%ProgramFiles%\DbgAtlas\etc\runtime.toml`：

```toml
version = 1
[server]
bind = "127.0.0.1:7331"
management_bind = "127.0.0.1:7332"
management_enabled = true
```

旧配置缺失管理字段时使用上述默认值。修改安装态配置后需重启服务。两个端口必须独立，管理端口绑定失败会使服务启动失败，不静默换端口；`management_enabled=false` 可关闭管理端。

## 页面流程

1. 从已登记项目下拉框选择项目，或输入标准项目根目录。页面默认打开最近活动项目，并支持通过 URL 直接定位。
2. 按会话、状态、命令、ID、摘要或创建时间范围筛选，默认每页 40 条。时间采用本地输入，查询使用毫秒时间戳和左闭右开区间。
3. 打开操作详情，查看输入、上下文和执行历史。选择产物可查看生产者和引用会话。文本输出按最多 64 KiB 分段读取。
4. 调试或采集后点击刷新。页面不自动发送调试命令或取消操作。

原始输出按文本显示。项目离线或文件缺失时仍可查询执行结果与归档错误，读取正文会报告具体错误。缺少现场证据的非终态记录显示 unknown。

## 服务边界

- `POST /api/query` 与 MCP `history.query` 共用只读查询，支持 overview、workspace、history、provenance、output 和 facts。历史按 sequence 续读，每页最多 100 条。
- 查询可传 project_root 或已登记 workspace_root，output 仅接受已登记 Artifact ID，通过 Workspace 的受控路径与文件句柄检查读取，不接受任意文件路径。
- 管理端不转发通用 `/rpc` 或 `/mcp`。请求检查精确本机 Host、同源 Origin 和 Fetch Metadata；API 要求 JSON Content-Type，不开放 CORS。页面使用 CSP、禁止嵌入且通过 textContent 展示记录。
- 与原服务一样信任本机调用者，无用户认证；loopback 和浏览器请求校验不构成本机用户隔离。
- 会话和时间筛选在 SQLite 内完成，分页后读取本页引用的产物元数据。当前服务掌握的运行态和归档错误优先显示。

## 后续里程碑

M3 验证异常退出、文件归档中断和派生索引重建。M1 浏览器验收记录仅适用于其当时版本。

历史验证见 [M1 管理页验收](management-mvp-validation.md)。M2 见 [中心库验收](management-m2-validation.md)。
