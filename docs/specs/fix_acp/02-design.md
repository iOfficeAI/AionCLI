# ACP 协议集成：设计方案

> **范围**: 仅 `crates/aionui-ai-agent` 内部变更 **约束**: 遵守 AGENTS.md Architecture Rules — 特别是 crate 层级、domain crate 结构、依赖方向。

---

## 1. 层级关系

变更完全在 `aionui-ai-agent` domain crate 内部完成。 不触碰 foundation crate（`common`、`api-types`、`db`）。 不创建新 crate — ACP 协议支持是 `ai-agent` 的子功能。

```
                   不变                             变更
               ┌──────────────┐             ┌─────────────────────┐
 routes.rs     │  acp_routes  │             │                     │
               │  (handlers)  │             │                     │
               └──────┬───────┘             │                     │
                      │ downcast            │                     │
               ┌──────▼───────┐             │                     │
 相当于         │ AcpAgent-    │◄────────────┤  重写内部实现         │
 service.rs    │   Manager    │             │  (session 逻辑,     │
               │              │             │   协议调用)          │
               └──────┬───────┘             │                     │
                      │                     │                     │
            ┌─────────▼──────────┐          │                     │
 新文件      │  acp_protocol.rs   │◄─────────┤  新增模块            │
            │  (SDK 集成)        │          │                     │
            └─────────┬──────────┘          │                     │
                      │                     │                     │
               ┌──────▼───────┐             │                     │
 已有文件       │ CliAgent-    │◄────────────┤  小幅适配            │
               │   Process    │             │  (暴露 raw stdio)   │
               └──────────────┘             └─────────────────────┘
```

### 各文件职责（变更后）

| 文件 | 职责 | 是否引入 SDK？ |
| --- | --- | --- |
| `cli_process.rs` | 进程生命周期：spawn, kill, is_running, wait_for_exit, stderr 捕获 | 否 |
| `acp_protocol.rs` | ACP JSON-RPC：initialize 握手、类型化请求/响应、agent→client handler 分发 | 是 |
| `acp_agent.rs` | 业务编排：session 策略、状态机、confirmation 管理、IAgentManager 实现 | 否 |
| `acp_error.rs` | ACP 专用错误类型，含错误码分类 | 仅引入 `agent_client_protocol::ErrorCode` |
| `stream_event.rs` | `AgentStreamEvent` enum 定义（不变） + SDK `SessionNotification` 转换函数 | 引入 SDK schema 类型用于转换 |

SDK 依赖控制在最小范围 — 只有 `acp_protocol.rs`、`acp_error.rs` 和 `stream_event.rs` 的转换层接触它。

## 2. `cli_process.rs` — 适配

### 变更内容

1. **新增** `take_stdio()` — 返回 `(ChildStdin, ChildStdout)` 的所有权， 让 SDK 的 `ByteStreams` 接管。调用后 `send()` 不再可用（stdin 已转移）。

2. **保留 stderr 捕获** — stderr 仍由 tokio 后台任务读取，缓冲最后 N 字节 用于错误诊断。这是纯进程管理，不涉及协议。

3. **移除 stdout JSON 解析** — 原来的 raw `serde_json::Value` broadcast channel、 `event_tx`、`initial_rx` 和 stdout reader 任务全部移到 `acp_protocol.rs`（由 SDK transport 接管）。

4. **其余不变** — `spawn()`、`kill()`、`is_running()`、`wait_for_exit()`、 `close_stdin()`、`force_kill()` 保持原样。

### 为什么

`CliAgentProcess` 是进程管理 — 它不应该知道 JSON-RPC、session 或 ACP。 当前它解析 stdout JSON，这属于协议层的工作。变更后它成为纯粹的进程包装器。

## 3. `acp_protocol.rs` — 新增模块

### 职责

持有 SDK `Builder<Client>`，执行 ACP 握手，提供所有 ACP 操作的类型化异步方法。 同时注册 agent→client 回调 handler（`session/update`、`session/request_permission`、 `fs/read_text_file`、`fs/write_text_file`）。

### 公共 API

```rust
pub struct AcpProtocol { /* opaque */ }

impl AcpProtocol {
    /// 连接到运行中的 CLI 进程并执行 ACP initialize 握手。
    ///
    /// 接管 CliAgentProcess 的 stdin/stdout 所有权。
    /// 启动 SDK 后台任务处理 JSON-RPC 消息路由。
    /// initialize 握手完成后返回。
    pub async fn connect(
        stdin: ChildStdin,
        stdout: ChildStdout,
        event_tx: broadcast::Sender<AgentStreamEvent>,
        permission_tx: mpsc::Sender<PermissionRequest>,
    ) -> Result<Self, AcpError>;

    /// 创建新的 ACP session。
    pub async fn new_session(&self, params: NewSessionParams) -> Result<SessionId, AcpError>;

    /// 恢复/加载已有 session。
    pub async fn load_session(&self, session_id: &str, params: LoadSessionParams) -> Result<SessionId, AcpError>;

    /// 向活跃 session 发送 prompt。阻塞直到 prompt response 到达（本轮结束）。
    pub async fn prompt(&self, session_id: &str, content: PromptContent) -> Result<(), AcpError>;

    /// 取消当前 prompt。
    pub async fn cancel(&self, session_id: &str) -> Result<(), AcpError>;

    /// 设置 session mode。
    pub async fn set_mode(&self, session_id: &str, mode: &str) -> Result<(), AcpError>;

    /// 设置 session model。
    pub async fn set_model(&self, session_id: &str, model_id: &str) -> Result<(), AcpError>;

    /// 设置 session 配置选项。
    pub async fn set_config_option(&self, session_id: &str, key: &str, value: &str) -> Result<(), AcpError>;

    /// SDK 连接是否仍然存活。
    pub fn is_connected(&self) -> bool;
}
```

### 内部设计

```rust
struct AcpProtocol {
    /// SDK 连接句柄，用于向 agent 发送请求。
    connection: ConnectionTo<Agent>,
    /// 后台任务 handle（SDK transport + routing）。
    _bg_task: JoinHandle<()>,
    /// 存活标志，SDK 连接关闭时置为 false。
    alive: Arc<AtomicBool>,
}
```

`connect()` 构造过程：

1. 用 `tokio_util::compat` 将 `ChildStdin`/`ChildStdout` 适配为 `futures::AsyncRead` / `futures::AsyncWrite`。
2. 构建 `ByteStreams::new(write, read)`。
3. 使用 `Client.builder()` 注册 handler：
   - `on_receive_notification` 处理 `SessionNotification` → 转为 `AgentStreamEvent`，通过 `event_tx` 发送。
   - `on_receive_request` 处理 `RequestPermissionRequest` → 转发到 `permission_tx` channel，等待 `AcpAgentManager` 回复，再通过 `Responder` 返回。
   - `on_receive_request` 处理 `ReadTextFileRequest` / `WriteTextFileRequest` → 实现或拒绝。
4. 调用 `connect_with(transport, |cx| { ... })` 执行 `initialize`。
5. 将 `cx`（即 `ConnectionTo<Agent>`）存入 `self.connection`。

### Handler callback → AgentStreamEvent

SDK 将 `SessionNotification` 传递给我们的 handler。我们定义转换函数：

```rust
fn session_notification_to_events(notif: &SessionNotification) -> Vec<AgentStreamEvent>
```

放在 `stream_event.rs`，让事件定义和转换逻辑在一起。将 SDK session update 字段映射到现有的 `AgentStreamEvent` 变体。未知/无法映射的字段记录日志并跳过 — 不 panic，不静默吞掉。

### 权限流程

ACP `requestPermission` 是一个 JSON-RPC *request* — agent 期望收到 response。

```
Agent ──RequestPermissionRequest──► acp_protocol.rs handler
                                        │
                                        ▼
                                   permission_tx.send(PermissionRequest { responder, ... })
                                        │
                                        ▼
                                   AcpAgentManager 收到后, 向 UI 发出 AcpPermission 事件
                                        │
                              (用户通过 HTTP confirm 端点批准/拒绝)
                                        │
                                        ▼
                                   AcpAgentManager 调用 responder.respond(...)
                                        │
                                        ▼
Agent ◄──RequestPermissionResponse──  SDK 发送 JSON-RPC response
```

替代当前 fire-and-forget 的 `confirmMessage` stdin 写入。

## 4. `acp_agent.rs` — 重写内部

### 变更内容

- 持有 `AcpProtocol` 而非 `Arc<CliAgentProcess>` 用于协议操作。
- 仍持有 `Arc<CliAgentProcess>` 用于进程生命周期管理（kill, is_running）。
- `session_new()` 调用 `self.protocol.new_session(params).await?`而非手动构造 JSON。
- `session_resume_and_send()` 调用 `self.protocol.load_session(...)` 然后 `self.protocol.prompt(...)`。
- `send_message_to_process()` 调用 `self.protocol.prompt(...)`。
- `run_event_relay()` 被 SDK handler 回调替代 — 事件通过 `acp_protocol.rs`填充的 `event_tx` broadcast channel 到达。
- `confirm()` 通过权限请求中存储的 `Responder` 发送响应，而非 stdin JSON。
- `kill()` 仍委托给 `CliAgentProcess::kill()`。

### 不变的部分

- `AcpState` 结构体及其字段
- `SessionResumeStrategy` enum 和 `for_backend()` 逻辑
- `yolo_mode_value()` helper
- `IAgentManager` 实现签名
- `approval_key()`、`add_confirmation()`、`remove_confirmation()`
- `acp_routes.rs` 中的 downcast 模式

## 5. `acp_error.rs` — 新增模块

完整设计见 [03-error-model.md](03-error-model.md)。

## 6. Unstable Features

以下 SDK feature 需要在 `Cargo.toml` 中启用：

| Feature | 使用方 | 原因 |
| --- | --- | --- |
| `unstable_session_model` | `set_model()` | 模型切换 |
| `unstable_session_close` | `close_session()` | session 清理关闭 |

其他 unstable feature（`session_fork`、`session_resume`、`session_usage`） **暂不启用**，等有具体需求再开。`load_session` 是 stable 方法。

## 7. 依赖方向验证

```
acp_protocol.rs  →  agent-client-protocol (外部 crate, OK)
acp_protocol.rs  →  cli_process.rs (同 crate, OK)
acp_protocol.rs  →  stream_event.rs (同 crate, OK)
acp_protocol.rs  →  acp_error.rs (同 crate, OK)
acp_agent.rs     →  acp_protocol.rs (同 crate, OK)
acp_agent.rs     →  cli_process.rs (同 crate, OK)
acp_agent.rs     →  acp_error.rs (同 crate, OK)
acp_error.rs     →  agent-client-protocol::ErrorCode (外部, OK)
acp_error.rs     →  aionui-common::AppError (foundation, OK — domain→foundation 允许)
stream_event.rs  →  agent-client-protocol::schema types (外部, OK)
```

无向上依赖。无循环依赖。无 foundation crate 变更。

## 8. 迁移路径

变更完全在 `aionui-ai-agent` 内部。外部接口保持不变：

| 接口 | 状态 |
| --- | --- |
| `IAgentManager` trait | 不变 |
| `AgentManagerHandle` type alias | 不变 |
| `AgentStreamEvent` enum | 不变（新增转换函数） |
| `AcpAgentManager` 公共方法 | 签名不变，内部重写 |
| `acp_routes.rs` handlers | 不变 |
| `factory.rs` agent 构造 | 不变（`AcpAgentManager::new()` 签名可能有小幅参数调整） |
| `CliAgentProcess` 公共 API | 新增 `take_stdio()`；移除 stdout 解析 |
| `CliSpawnConfig` | 不变 |

现有的 `AcpAgentManager` 状态转换和事件解析测试需要适配，因为事件现在通过 SDK 回调到达而非 raw JSON broadcast。测试*断言*保持同等具体 — 只是 setup 变化。