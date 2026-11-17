# 本机管理页面 MVP

管理页面是迭代 4（会话与操作记录）和迭代 5（可视化）的共同 M1。页面先接入真实 JSONL 和 session.json；SQLite 元数据存储与旧记录迁移属于 M2。完整输出继续保存在 workspace 文件中，Markdown 用于分析笔记和报告。

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

1. 输入项目目录，例如 `D:\Cases\case-001`，读取 `<project_root>\dbgatlas` 下的已有 workspace。页面不创建目录，也不扫描整块磁盘。最近项目保存在当前浏览器本地，选择器也包含本次服务已知项目。
2. 浏览会话、操作与产物数量，按会话、状态或命令/ID/摘要文本筛选；操作列表每页 40 项。
3. 打开操作详情，查看输入、执行上下文和产物引用。文本输出按最多 64 KiB 分段加载，保留 UTF-8 边界；二进制产物展示引用及路径，不尝试文本解码。
4. 调试或采集后点击刷新。页面不自动发送调试命令或取消操作。

原始输出作为文本显示，文件中的 HTML/Markdown 不执行。文件缺失显示读取错误，非法 UTF-8 显示提示。历史非终态在缺少当前运行期操作时投影为 unknown；同实例已知结果优先于旧 JSONL，输出归档错误单独展示。关闭会话不删除其文件，重启服务后仍可显式打开历史 workspace。

## 服务边界

- `/`、`/app.js`、`/style.css` 提供内嵌页面；`POST /api/query` 提供 `overview`、`workspace`、`output` 三种只读查询。
- query 使用显式 project_root，output 仅接受已登记 Artifact ID，通过 Workspace 的受控路径与文件句柄检查读取，不接受任意文件路径。
- 管理端不转发通用 `/rpc` 或 `/mcp`。请求检查精确本机 Host、同源 Origin 和 Fetch Metadata；API 要求 JSON Content-Type，不开放 CORS。页面使用 CSP、禁止嵌入且通过 textContent 展示记录。
- 与原服务一样信任本机调用者，无用户认证；loopback 和浏览器请求校验不构成本机用户隔离。
- 页面查询复用 Workspace 记录、ServiceHost 状态和 Artifact 读取。MVP 会扫描 JSONL，并对结果分页；这是过渡实现，不能推断为大型工作区索引性能已通过验收。

## 后续里程碑

M2 将结构化会话、操作、来源关系迁入 SQLite，完整输出仍保留文件；补齐迁移、版本管理、索引和时间筛选。M3 完善迁移中断、归档失败等故障验收。全文检索、网页命令执行、远端节点和稳定目标跨会话匹配另按 PLAN.md 推进。

本轮测试、安装态更新、实际浏览器检查与未覆盖项见[管理 MVP 验证记录](management-mvp-validation.md)。
