# ACP 架构分析

## 1. 整体架构概览

aionui-backend 是 AionUI 的后端服务，基于 **Axum + Tokio + SQLite** 构建，采用 Cargo workspace 组织，包含 17 个 crate，分为四层架构。

### 分层结构

```
┌─────────────────────────────────────────────────────────────┐
│  组合层 (Composition)                                       │
│  ┌─────────────────────────────────────────────────────────┐│
│  │  aionui-app                                             ││
│  │  (二进制入口, 路由组装, 依赖注入编排)                   ││
│  └─────────────────────────────────────────────────────────┘│
├─────────────────────────────────────────────────────────────┤
│  领域层 (Domain) — 11 个 crate, 松耦合                      │
│  ┌────────────┬────────────┬────────────┬────────────┐      │
│  │conversation│  channel   │   team     │   cron     │      │
│  ├────────────┼────────────┼────────────┼────────────┤      │
│  │ ai-agent   │   file     │  office    │  system    │      │
│  ├────────────┼────────────┼────────────┼────────────┤      │
│  │    mcp     │ extension  │   shell    │            │      │
│  └────────────┴────────────┴────────────┴────────────┘      │
├─────────────────────────────────────────────────────────────┤
│  能力层 (Capability) — 横切关注点                           │
│  ┌───────────────────────┬─────────────────────────────┐    │
│  │  aionui-auth          │  aionui-realtime            │    │
│  │  (JWT, CSRF, bcrypt,  │  (WebSocket, 事件总线,      │    │
│  │   认证中间件)         │   广播通道)                 │    │
│  └───────────────────────┴─────────────────────────────┘    │
├─────────────────────────────────────────────────────────────┤
│  基础层 (Foundation) — 零/极少内部依赖                      │
│  ┌─────────────────┬────────────────┬──────────────────┐    │
│  │  aionui-common  │ aionui-api-    │  aionui-db       │    │
│  │  (AppError,     │  types         │  (SQLite, sqlx,  │    │
│  │   枚举, 加密,   │  (请求/响应    │   仓储 trait     │    │
│  │   ID 生成,      │   DTO)         │   及实现)        │    │
│  │   分页)         │                │                  │    │
│  └─────────────────┴────────────────┴──────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

### 依赖方向规则

```
组合层 → 领域层 → 能力层 → 基础层
         领域层 → 基础层   (允许跨层)
```

- 上层可以依赖下层
- 同层交互仅通过 trait 抽象（例如 `IWorkerTaskManager`）
- 禁止循环依赖，禁止向上引用

### 核心架构模式

| 模式                  | 说明                                                                                   |
| --------------------- | -------------------------------------------------------------------------------------- |
| 基于 Trait 的依赖注入 | 仓储 trait 在 `aionui-db` 定义，服务 trait 在领域 crate 定义，统一在 `aionui-app` 装配 |
| 集中式 AppServices    | 所有共享服务的唯一构造中心                                                             |
| 事件驱动实时推送      | `BroadcastEventBus` + WebSocket 实现客户端实时更新                                     |
| 领域隔离              | 每个领域 crate 独立拥有 `routes.rs` / `service.rs` / `state.rs`                        |
| Worker 任务队列       | `IWorkerTaskManager` 管理每个会话的 agent 生命周期                                     |

### 领域 Crate 标准结构

```
crates/aionui-{domain}/src/
├── lib.rs       # 仅模块导出，不含业务逻辑
├── routes.rs    # HTTP handler（请求/响应转换）
├── service.rs   # 业务逻辑（唯一位置）
├── state.rs     # RouterState（#[derive(Clone)]，Arc 包裹的依赖）
├── error.rs     # 领域特定错误（可选）
└── types.rs     # 领域模型（可选）
```

---

## 2. ACP 在整体架构中的位置

### 定位

ACP（Agent Communication Protocol）是 AionUI 的**核心 Agent 编排子系统**。它主要位于 **aionui-ai-agent** 领域 crate，同时跨越多个关联区域：

```
┌───────────────────────────────────────────────────────────────────┐
│                        aionui-app                                 │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │  路由装配（ACP 路由, 远程 Agent 路由, 连接测试路由等）     │   │
│  │  AppServices → 构建 AcpRouterState, ConnectionTestState    │   │
│  └────────────────────────────────────────────────────────────┘   │
├───────────────────────────────────────────────────────────────────┤
│                       领域层                                      │
│                                                                   │
│  ┌──────────────────────────────────────────────────────────┐     │
│  │               aionui-ai-agent  ◀── ACP 核心              │     │
│  │  ┌──────────────────────────────────────────────────┐    │     │
│  │  │  ACP Agent      │  Remote Agent   │  Agent       │    │     │
│  │  │  (acp_agent.rs) │ (remote_agent.rs│  Factory     │    │     │
│  │  │  (acp_routes.rs)│  remote_agent_  │ (factory.rs) │    │     │
│  │  │  (acp_service)  │  routes/service)│              │    │     │
│  │  ├──────────────────────────────────────────────────┤    │     │
│  │  │  Task Manager   │  Stream Events  │  CLI Process │    │     │
│  │  │ (task_manager)  │(stream_event)   │(cli_process) │    │     │
│  │  ├──────────────────────────────────────────────────┤    │     │
│  │  │  Skill Manager  │  Middleware     │  Idle Scanner│    │     │
│  │  │ (skill_manager) │ (middleware)    │(idle_scanner)│    │     │
│  │  ├──────────────────────────────────────────────────┤    │     │
│  │  │  其他 Agent: Gemini, Nanobot, OpenClaw, Aionrs   │    │     │
│  │  └──────────────────────────────────────────────────┘    │     │
│  └──────────────────────────────────────────────────────────┘     │
│                                                                   │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐    │
│  │  conversation   │  │     team        │  │     cron        │    │
│  │ (通过 IWorker-  │  │ (通过 IWorker-  │  │ (通过 IWorker-  │    │
│  │  TaskManager    │  │  TaskManager    │  │  TaskManager    │    │
│  │  使用 Agent)    │  │  使用 Agent)    │  │  使用 Agent)    │    │
│  └─────────────────┘  └─────────────────┘  └─────────────────┘    │
├───────────────────────────────────────────────────────────────────┤
│  基础层:                                                          │
│    aionui-common    — AgentType, AcpBackend 等枚举                │
│    aionui-api-types — ACP 请求/响应 DTO                           │
│    aionui-db        — RemoteAgentRepository, OAuthTokenRepository │
└───────────────────────────────────────────────────────────────────┘
```

### 跨 Crate 依赖关系

ACP 相关代码涉及以下 crate：

| Crate                 | ACP 相关内容                                                                                                                           |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `aionui-common`       | `AgentType`, `AcpBackend`, `RemoteAgentProtocol`, `RemoteAgentAuthType`, `RemoteAgentStatus`, `AgentKillReason`, `Confirmation` 等类型 |
| `aionui-api-types`    | ACP 请求/响应 DTO（`acp.rs`, `remote_agent.rs`, `connection_test.rs`, `confirmation.rs`）                                              |
| `aionui-db`           | `IRemoteAgentRepository`, `IOAuthTokenRepository`, `RemoteAgentRow`, `OAuthTokenRow`, `acp_session` 表                                 |
| `aionui-ai-agent`     | ACP 核心实现（agent manager、路由、服务、工厂、任务管理器）                                                                            |
| `aionui-conversation` | 通过 `IWorkerTaskManager` 编排 agent 任务，实现消息收发                                                                                |
| `aionui-team`         | 多 agent 会话管理，使用 agent 工厂构建团队成员                                                                                         |
| `aionui-cron`         | 定时触发 agent 调用，通过 `IWorkerTaskManager`                                                                                         |
| `aionui-app`          | 装配所有 ACP 相关的 State 和路由                                                                                                       |

---

## 3. ACP 子系统详细架构

### 3.1 Agent 类型体系

AionUI 支持多种 agent 类型，统一在 `IAgentManager` trait 下：

```
                    IAgentManager (trait)
                          │
          ┌───────────────┼───────────────────────┐
          │               │                       │
    AcpAgentManager  RemoteAgentManager    GeminiAgentManager
    (CLI 子进程)     (WebSocket)           (CLI 子进程)
          │                                       │
    NanobotAgentManager  OpenClawAgentManager  AionrsAgentManager
    (CLI 子进程)         (WebSocket)           (CLI 子进程)
```

```rust
pub enum AgentType {
    Acp,               // CLI 类 agent（20+ 后端）
    Remote,            // WebSocket 远程 agent
    Gemini,            // Google Gemini CLI
    OpenclawGateway,   // OpenClaw WebSocket 协议
    Nanobot,           // Nanobot CLI
    Aionrs,            // Aionrs CLI
}
```

### 3.2 ACP 后端生态

ACP（`AgentType::Acp`）是主要的 agent 类型，支持 20+ CLI 后端：

```
AcpBackend
├── CLI 类（需要本地 PATH 中有对应二进制）
│   ├── Claude        (claude)
│   ├── Codex         (codex)
│   ├── CodeBuddy     (codebuddy)
│   ├── Qwen          (qwen)
│   ├── Kiro          (kiro)
│   ├── OpenCode      (opencode)
│   ├── Copilot       (copilot)
│   ├── Goose         (goose)
│   ├── Cursor        (cursor)
│   ├── Droid         (droid)
│   ├── Auggie        (auggie)
│   ├── Kimi          (kimi)
│   ├── Qoder         (qoder)
│   ├── Vibe          (vibe)
│   ├── Nanobot       (nanobot)
│   ├── Hermes        (hermes)
│   └── Snow          (snow)
│
├── 非 CLI 类（特殊处理）
│   ├── IFlow           (独立处理)
│   ├── Gemini          (由 GeminiAgentManager 处理)
│   ├── OpenclawGateway (由 OpenClawAgentManager 处理)
│   ├── Remote          (由 RemoteAgentManager 处理)
│   └── Aionrs          (由 AionrsAgentManager 处理)
│
└── Custom            (用户自定义命令)
```

### 3.3 IAgentManager Trait（核心接口）

所有 agent 类型实现此统一接口：

```rust
pub trait IAgentManager: Send + Sync {
    // 基本信息
    fn agent_type(&self) -> AgentType;
    fn status(&self) -> Option<ConversationStatus>;
    fn workspace(&self) -> &str;
    fn conversation_id(&self) -> &str;
    fn last_activity_at(&self) -> TimestampMs;

    // 事件订阅（广播通道）
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent>;

    // 消息操作
    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError>;
    async fn stop(&self) -> Result<(), AppError>;

    // 工具确认流程
    fn confirm(&self, msg_id: &str, call_id: &str, data: Value, always_allow: bool) -> Result<(), AppError>;
    fn get_confirmations(&self) -> Vec<Confirmation>;
    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool;

    // 生命周期
    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError>;
    fn as_any(&self) -> &dyn Any;  // 向下转型到具体类型
}
```

---

## 4. Agent 发现

### 4.1 CLI 检测

通过 `which` crate 在系统 PATH 中查找 CLI 二进制文件：

```
用户请求 → /api/acp/agents → acp_service::get_available_agents()
                                    │
                                    ├─ 遍历预定义的已知 agent 列表:
                                    │    cli_binary_name(backend) → Option<二进制名>
                                    │    which::which(binary_name) → available: bool
                                    │
                                    └─ 返回 Vec<AcpAgentInfo> { id, name, backend, available }
```

**相关端点：**

| 端点                      | 方法 | 说明                      |
| ------------------------- | ---- | ------------------------- |
| `/api/acp/agents`         | GET  | 列出已知 agent 及可用状态 |
| `/api/acp/agents/refresh` | POST | 重新扫描 agent 可用性     |
| `/api/acp/detect-cli`     | POST | 检测特定后端的 CLI 路径   |
| `/api/acp/agents/test`    | POST | 测试自定义 agent 命令     |
| `/api/acp/health-check`   | POST | 检查后端可用性 + 延迟     |
| `/api/acp/env`            | GET  | 获取相关环境变量          |

### 4.2 远程 Agent 发现

远程 agent 由用户配置并持久化到数据库：

```
用户通过 POST /api/remote-agents 创建
    │
    ├─ 参数: name, protocol (OpenClaw/ZeroClaw/Acp), url, auth_type
    │
    ├─ 若 protocol == OpenClaw:
    │    自动生成 Ed25519 设备密钥对
    │    加密后存储设备密钥
    │
    └─ 存入 remote_agents 表（auth_token 经 AES 加密）
```

---

## 5. Agent 认证

### 5.1 ACP CLI Agent

CLI 类 ACP agent 继承宿主系统的认证。CLI 二进制自身管理 API key 和凭证，AionUI 不直接处理本地 CLI agent 的认证。

### 5.2 远程 Agent 认证

三种认证方式：

| 认证类型   | 机制                                          |
| ---------- | --------------------------------------------- |
| `Bearer`   | Token 认证，auth_token 作为 bearer token 发送 |
| `Password` | 密码认证，auth_token 中存储密码               |
| `None`     | 无需认证                                      |

所有敏感数据在数据库中使用 AES-GCM 加密存储：

| 字段                 | 说明                               |
| -------------------- | ---------------------------------- |
| `auth_token`         | 认证令牌，AES 加密                 |
| `device_public_key`  | Ed25519 公钥（OpenClaw），AES 加密 |
| `device_private_key` | Ed25519 私钥（OpenClaw），AES 加密 |
| `device_token`       | 设备令牌（OpenClaw），AES 加密     |

加密密钥：由 JWT secret 派生的 32 字节密钥，通过 `AppServices` 传递。

### 5.3 OpenClaw 握手协议

```
POST /api/remote-agents/{id}/handshake
    │
    ├─ WebSocket 连接到远程 agent URL（15 秒超时）
    ├─ 基于设备密钥对的握手
    ├─ 成功后: status = "connected", last_connected_at = now
    └─ 返回 HandshakeResponse { status: "ok" }
```

### 5.4 连接测试

| 端点                                      | 说明                                          |
| ----------------------------------------- | --------------------------------------------- |
| `POST /api/remote-agents/test-connection` | WebSocket 连接测试（10 秒超时），含 SSRF 防护 |
| `POST /api/bedrock/test-connection`       | AWS Bedrock 凭证验证，隔离的 AWS SDK 配置     |
| `GET /api/gemini/subscription-status`     | Gemini 订阅状态查询（通过 GEMINI_API_KEY）    |

---

## 6. Agent 会话编排管理

### 6.1 任务管理器（每会话 Agent 生命周期）

`WorkerTaskManager` 维护会话到 agent 的一对一映射：

```
DashMap<conversation_id, AgentManagerHandle>
    │
    ├─ get_task(conv_id) → Option<Handle>            // 获取已有任务
    ├─ get_or_build_task(conv_id, options) → Handle  // 惰性创建
    ├─ kill(conv_id, reason) → ()                    // 终止并清理
    ├─ clear() → ()                                  // 终止所有
    ├─ active_count() → usize                        // 活跃数量
    └─ collect_idle(threshold_ms) → Vec<conv_id>     // 收集空闲任务（供 idle scanner 使用）
```

**关键约束：** 每个会话最多一个活跃 agent，防止资源浪费。

### 6.2 Agent 工厂（构建管线）

```
BuildTaskOptions
├── agent_type: AgentType
├── workspace: String（工作目录）
├── model: ProviderWithModel
├── conversation_id: String
└── extra: serde_json::Value（类型特定配置）
        │
        ├── AcpBuildExtra（AgentType::Acp）
        │   ├── backend: AcpBackend            // 后端类型
        │   ├── cli_path: Option<String>       // CLI 路径
        │   ├── agent_name: Option<String>     // Agent 名称
        │   ├── custom_workspace: bool         // 自定义工作目录
        │   ├── preset_context: Option<String> // 预设上下文
        │   ├── enabled_skills: Vec<String>    // 启用的技能
        │   ├── session_mode: Option<String>   // 会话模式
        │   └── cron_job_id: Option<String>    // 关联的定时任务 ID
        │
        ├── RemoteBuildExtra（AgentType::Remote）
        │   └── remote_agent_id: String
        │
        └── GeminiBuildExtra（AgentType::Gemini）
            └── (特定字段)
```

**工厂分发逻辑：**

```
match agent_type {
    Acp    → 解析 AcpBuildExtra → 启动 CLI 子进程 → AcpAgentManager
    Remote → 解析 RemoteBuildExtra → 查 DB → 解密凭证 → RemoteAgentManager → connect()
    Gemini → 类似 CLI 启动模式 → GeminiAgentManager
    ...
}
```

### 6.3 ACP Agent 会话生命周期

```
┌─────────────────────────────────────────────────────────────┐
│  AcpAgentManager 生命周期                                   │
│                                                             │
│  1. new() ─── 启动 CLI 子进程                               │
│       │       预订阅事件接收器（保证不丢失事件）            │
│       │       初始化 AcpState { status: None }              │
│       │                                                     │
│  2. start_relay() ─── 后台任务读取 CLI stdout               │
│       │                解析 AgentStreamEvent                │
│       │                更新内部状态                         │
│       │                广播给订阅者                         │
│       │                                                     │
│  3. send_message() ─── 获取 session_lock                    │
│       │                                                     │
│       ├── 首条消息: ensure_session_and_send()               │
│       │   ├── session/new（携带初始上下文）                 │
│       │   │   ├── preset_context 注入                       │
│       │   │   ├── enabled_skills 注入                       │
│       │   │   └── session_mode 覆盖                         │
│       │   └── status → Running                              │
│       │                                                     │
│       ├── 后续消息（按后端类型选择恢复策略）:               │
│       │   ├── SessionLoad (Codex):                          │
│       │   │   session/load → sendMessage                    │
│       │   ├── ClaudeResumeMeta (Claude/CodeBuddy):          │
│       │   │   session/new 带 resume 元数据                  │
│       │   └── ResumeSessionId (其他):                       │
│       │       session/new 带 resumeSessionId                │
│       │                                                     │
│  4. 事件流 ─── 持续                                         │
│       │   Start → 设置 session_id                           │
│       │   Text → 增量内容                                   │
│       │   AcpPermission → 加入待确认列表                    │
│       │   AcpModelInfo → 存储模型信息                       │
│       │   Finish → 设置 session_id, status → Finished       │
│       │   Error → status → Finished                         │
│       │                                                     │
│  5. confirm() ─── confirmMessage 协议命令                   │
│       │           从待确认列表移除                          │
│       │           若 always_allow: 存入 approval_memory     │
│       │                                                     │
│  6. stop() ─── session/cancel                               │
│                                                             │
│  7. kill() ─── 终止 CLI 进程（500ms 优雅期）                │
│               从 task manager 清除                          │
└─────────────────────────────────────────────────────────────┘
```

### 6.4 会话恢复策略

不同后端使用不同的会话恢复机制：

| 策略               | 适用后端          | 机制                                                 |
| ------------------ | ----------------- | ---------------------------------------------------- |
| `SessionLoad`      | Codex             | `session/load` + `sendMessage`                       |
| `ClaudeResumeMeta` | Claude, CodeBuddy | `session/new` 携带 `_meta.claudeCode.options.resume` |
| `ResumeSessionId`  | 其他所有          | `session/new` 携带 `resumeSessionId`                 |

### 6.5 每会话控制端点

| 端点                                            | 方法    | 说明                |
| ----------------------------------------------- | ------- | ------------------- |
| `/api/conversations/{id}/acp/mode`              | GET/PUT | 获取/设置 YOLO 模式 |
| `/api/conversations/{id}/acp/model`             | GET/PUT | 获取/设置模型       |
| `/api/conversations/{id}/acp/config`            | GET     | 获取配置选项        |
| `/api/conversations/{id}/acp/config/{configId}` | PUT     | 设置配置选项        |

**YOLO 模式**（绕过权限确认）各后端差异：

| 后端               | mode 值               |
| ------------------ | --------------------- |
| Claude / CodeBuddy | `"bypassPermissions"` |
| Qwen / IFlow       | `"yolo"`              |
| 其他               | 不支持                |

---

## 7. ACP 连接与通信

### 7.1 CLI 子进程通信（本地 ACP Agent）

```
┌──────────────┐    stdin (JSON)     ┌─────────────────┐
│ AionUI       │ ──────────────────▶ │ CLI 子进程      │
│ Backend      │                     │ (claude, qwen,  │
│              │ ◀────────────────── │  codex, ...)    │
│              │    stdout (JSON)    │                 │
└──────────────┘                     └─────────────────┘
```

**协议命令（通过 stdin 发送给子进程）：**

| 命令                       | 说明                     |
| -------------------------- | ------------------------ |
| `session/new`              | 创建新会话并发送初始消息 |
| `session/load`             | 加载已有会话（仅 Codex） |
| `session/cancel`           | 停止当前流式响应         |
| `sendMessage`              | 向已有会话发送消息       |
| `confirmMessage`           | 批准/拒绝工具调用        |
| `session/setMode`          | 设置模式（YOLO 等）      |
| `session/getMode`          | 查询当前模式             |
| `session/setModel`         | 切换模型                 |
| `session/getModelInfo`     | 获取当前模型信息         |
| `session/getConfigOptions` | 获取可用配置项           |
| `session/setConfigOption`  | 设置配置项               |
| `session/getSlashCommands` | 获取可用斜杠命令         |

**协议格式（stdin → 子进程）：**
```json
{
  "type": "session/new",
  "data": {
    "message": "用户消息",
    "workspace": "/path/to/workspace",
    "systemPrompt": "可选上下文",
    "_meta": { /* 后端特定元数据 */ }
  }
}
```

### 7.2 WebSocket 通信（远程 Agent）

```
┌──────────────┐   WebSocket (JSON)  ┌─────────────────┐
│ AionUI       │ ◀════════════════▶  │ 远程 Agent      │
│ Backend      │                     │ 服务端          │
└──────────────┘                     └─────────────────┘
```

远程 agent 通过 WebSocket 进行双向通信，复用与 CLI agent 相同的 `AgentStreamEvent` 类型。

### 7.3 流事件类型（24 种）

```rust
pub enum AgentStreamEvent {
    // 生命周期
    Start { session_id },      // 响应轮次开始
    Finish { session_id },     // 轮次完成
    Error { message, code },   // 处理错误

    // 内容
    Text { content },                                // 增量文本
    Thinking { content, subject, duration, status }, // 推理过程
    Plan { entries },                                // 执行计划

    // 工具交互
    ToolCall { call_id, name, args, status },   // 单个工具调用
    ToolGroup { calls },                        // 工具调用组
    AcpPermission { ... },                      // 工具审批请求 → Confirmation
    AcpToolCall { ... },                        // ACP 特定的工具进度
    CodexPermission { ... },                    // Codex 变体
    CodexToolCall { ... },                      // Codex 变体

    // 会话信息
    AgentStatus { backend, status, agent_name, session_id },  // 状态更新
    AcpModelInfo { model_id, model_name, provider },          // 模型信息
    AcpContextUsage { ... },                                  // Token/上下文使用指标

    // 功能
    Tips { level, message },                // 通知（error/success/warning）
    AvailableCommands { commands },         // 可用斜杠命令
    SkillSuggest { skill_name },            // 建议启用的技能
    CronTrigger { cron_job_id },            // 定时任务通知
    System { content },                     // 系统消息
    RequestTrace { ... },                   // 调试追踪数据
}
```

---

## 8. 工具确认与审批流程

### 8.1 确认生命周期

```
CLI 发出 AcpPermission 事件
    │
    ▼
事件中继解析 → Confirmation { id, call_id, action, description, options }
    │
    ▼
加入 AcpState.confirmations 待确认列表
    │
    ▼
通过广播通道推送给 WebSocket 订阅者 → 前端展示审批对话框
    │
    ▼
用户操作
    │
    ├── 批准 (always_allow=false): confirmMessage → 从列表移除
    ├── 批准 (always_allow=true):  confirmMessage → 从列表移除
    │                               + 存入 approval_memory
    └── 拒绝: confirmMessage 携带拒绝值 → 从列表移除
```

### 8.2 审批记忆（会话级别）

```rust
// 审批 key 格式
fn approval_key(action, command_type) -> String {
    match (action, command_type) {
        (Some(a), Some(ct)) => format!("{a}:{ct}"),  // 如 "edit_file:bash"
        (Some(a), None) => a.to_owned(),             // 如 "read_file"
        _ => String::new(),
    }
}
```

- **作用域：** 会话级（kill 时清除，不跨重启持久化）
- **用途：** 对相同的工具确认请求自动批准，避免重复弹窗
- **检查：** 通过 `check_approval(action, command_type)` 在展示 UI 对话框前判断

---

## 9. 空闲清理与生命周期管理

### 9.1 Idle Scanner

```
start_idle_scanner(task_manager, interval_secs, idle_threshold_ms)
    │
    └── 周期性循环:
        ├── collect_idle(threshold_ms)
        │   └── 遍历所有活跃任务:
        │       若 agent_type == Acp
        │          且 status == Finished
        │          且 (now - last_activity) > threshold_ms
        │       → 加入空闲列表
        │
        └── 对每个空闲的 conversation_id:
            kill(conv_id, Some(IdleTimeout))
```

### 9.2 活跃度追踪

- `last_activity: AtomicI64` — 无锁，每次收到 CLI stdout 事件时更新
- `Ordering::Relaxed` — 对空闲检测足够精确（无需严格排序保证）

---

## 10. 数据库 Schema（ACP 相关）

### remote_agents 表

```sql
CREATE TABLE remote_agents (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    protocol          TEXT NOT NULL,          -- openClaw, zeroClaw, acp
    url               TEXT NOT NULL,          -- WebSocket 端点
    auth_type         TEXT NOT NULL,          -- bearer, password, none
    auth_token        TEXT,                   -- AES 加密
    allow_insecure    INTEGER NOT NULL DEFAULT 0,
    avatar            TEXT,
    description       TEXT,
    device_id         TEXT,                   -- OpenClaw 设备 ID
    device_public_key TEXT,                   -- AES 加密的 Ed25519 公钥
    device_private_key TEXT,                  -- AES 加密的 Ed25519 私钥
    device_token      TEXT,                   -- AES 加密
    status            TEXT NOT NULL DEFAULT 'unknown',
    last_connected_at INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
```

### acp_session 表

```sql
CREATE TABLE acp_session (
    conversation_id TEXT PRIMARY KEY,
    agent_backend   TEXT NOT NULL,            -- claude, qwen, codex 等
    agent_source    TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    session_id      TEXT,                          -- 后端会话 ID
    session_status  TEXT NOT NULL DEFAULT 'idle',  -- idle, running, suspended
    session_config  TEXT NOT NULL DEFAULT '{}',    -- JSON 配置
    last_active_at  INTEGER,
    suspended_at    INTEGER
);
```

### oauth_tokens 表

```sql
CREATE TABLE oauth_tokens (
    server_url    TEXT PRIMARY KEY,
    access_token  TEXT NOT NULL,
    refresh_token TEXT,
    token_type    TEXT NOT NULL DEFAULT 'bearer',
    expires_at    INTEGER,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
```

---

## 11. 并发模型

| 组件                      | 机制                      | 用途                            |
| ------------------------- | ------------------------- | ------------------------------- |
| `AcpState`                | `RwLock<AcpState>`        | 线程安全的状态读（多）/写（少） |
| `session_lock`            | `Mutex<()>`               | 串行化 session/new 和 send 操作 |
| `last_activity`           | `AtomicI64`               | 无锁时间戳，用于空闲检测        |
| `raw_rx`                  | `Mutex<Option<...>>`      | 预订阅接收器，恰好取出一次      |
| `WorkerTaskManager.tasks` | `DashMap`                 | 无锁的逐条目并发访问            |
| 事件广播                  | `broadcast::channel(256)` | 多订阅者事件分发                |

---

## 12. 消息中间件管线

```
用户消息输入
    │
    ├── 1. strip_think_tags()
    │      移除 <think>...</think> 和 <thinking>...</thinking>
    │
    ├── 2. detect_cron_commands()
    │      检测 [CRON_CREATE]...[/CRON_CREATE]
    │             [CRON_LIST]
    │             [CRON_DELETE: id]
    │
    ├── 3. 执行定时任务命令（若 ICronService 可用）
    │
    └── 输出: MiddlewareResult { cleaned_message, display_message, system_responses }
```

---

## 13. Skill 管理

`AcpSkillManager` 负责将技能上下文注入 ACP 会话：

| 功能                          | 说明                     |
| ----------------------------- | ------------------------ |
| `build_skills_index_text()`   | 构建可用技能的索引文本   |
| `build_system_instructions()` | 构建包含技能的系统提示词 |
| `prepare_first_message()`     | 用技能上下文丰富首条消息 |
| `detect_skill_load_request()` | 从消息中解析 /skill 命令 |

技能在 `session/new` 时通过以下方式注入：
- `AcpBuildExtra.preset_context` — 预设上下文
- `AcpBuildExtra.enabled_skills` — 启用的技能列表

---

## 14. 完整架构总图

```
┌─────────────────────────────────────────────────────────────────────────┐
│                       客户端（桌面应用）                                │
│                    REST API + WebSocket                                 │
└──────────────┬──────────────────────────────────┬───────────────────────┘
               │                                  │
         HTTP REST                          WebSocket
               │                                  │
┌──────────────▼──────────────────────────────────▼───────────────────────┐
│                           aionui-app                                    │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │  中间件栈: CORS → 安全头 → CSRF → 认证 → Handler                │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │  AppServices（集中式依赖注入）                                  │    │
│  │  → WorkerTaskManager（每会话 agent 缓存）                       │    │
│  │  → AgentFactory（按类型构建 agent）                             │    │
│  │  → BroadcastEventBus（实时事件）                                │    │
│  └─────────────────────────────────────────────────────────────────┘    │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  /api/acp/*                  → ACP 全局管理                             │
│  /api/conversations/*/acp/*  → 每会话控制                               │
│  /api/remote-agents/*        → 远程 Agent CRUD                          │
│  /api/bedrock/*              → AWS Bedrock 连接测试                     │
│  /api/gemini/*               → Gemini 订阅检查                          │
│                                                                         │
├─────────────────────────────────────────────────────────────────────────┤
│                       aionui-ai-agent                                   │
│                                                                         │
│  ┌─────────────────────────────────┐  ┌─────────────────────────────┐   │
│  │  WorkerTaskManager              │  │  AgentFactory               │   │
│  │   DashMap<conv_id, AgentHandle> │  │   fn(options) → AgentHandle │   │
│  └───────────────┬─────────────────┘  └──────────────┬──────────────┘   │
│                  │                                   │                  │
│  ┌───────────────▼────────────┐         ┌────────────▼──────────┐       │
│  │   AcpAgentManager          │         │  RemoteAgentManager   │       │
│  │   (CLI 子进程)             │         │  (WebSocket)          │       │
│  │   ┌─────────────────────┐  │         │  ┌─────────────────┐  │       │
│  │   │ AcpState            │  │         │  │ Connection      │  │       │
│  │   │ (RwLock)            │  │         │  │ (WS)            │  │       │
│  │   ├─────────────────────┤  │         │  ├─────────────────┤  │       │
│  │   │ EventRelay          │  │         │  │ EventRelay      │  │       │
│  │   │ (stdout→ broadcast) │  │         │  │ (ws→ broadcast) │  │       │
│  │   └─────────────────────┘  │         │  └─────────────────┘  │       │
│  └────────────────────────────┘         └───────────────────────┘       │
│                │                                     │                  │
│                ▼                                     ▼                  │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │                 AgentStreamEvent（24 种事件类型）                │   │
│  │           → 广播给 WebSocket 订阅者                              │   │
│  │           → 状态更新（session_id, confirmations 等）             │   │
│  └──────────────────────────────────────────────────────────────────┘   │
│                                                                         │
│  ┌──────────────────────┐  ┌──────────────────────┐                     │
│  │   IdleScanner        │  │   SkillManager       │                     │
│  │   (周期性空闲清理)   │  │   (上下文注入)       │                     │
│  └──────────────────────┘  └──────────────────────┘                     │
│                                                                         │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  ┌──────────────┐ ┌─────────────────────┐ ┌──────────────┐              │
│  │ CLI 子进程   │ │ WebSocket           │ │ AWS Bedrock  │              │
│  │ (subprocess) │ │ (tokio-tungstenite) │ │ (aws-sdk)    │              │
│  └──────┬───────┘ └──────┬──────────────┘ └───────┬──────┘              │
│         │                │                        │                     │
└─────────▼────────────────▼────────────────────────▼─────────────────────┘
    本地 CLI            远程 Agent            AWS Bedrock
    (claude, qwen,      服务端                   Runtime
     codex, ...)       (WebSocket)
```

---

## 15. 关键设计决策与权衡

| 决策                       | 理由                                                                       |
| -------------------------- | -------------------------------------------------------------------------- |
| 本地 agent 采用 CLI 子进程 | 复用现有 CLI 工具（claude, qwen 等），无需重新实现其协议；每个进程独立隔离 |
| 远程 agent 采用 WebSocket  | 双向流式通信，适合长寿命的 agent 会话                                      |
| 每会话 agent 缓存          | DashMap 提供无锁读取；每会话最多一个活跃 agent 防止资源浪费                |
| 审批记忆仅会话级           | 平衡用户体验（不重复询问）与安全性（不跨重启持久化）                       |
| AES-GCM 加密敏感 DB 字段   | 静态加密，密钥从 JWT secret 派生                                           |
| 后端特定的会话恢复策略     | 不同 CLI 有不兼容的会话管理；策略模式隔离差异                              |
| 广播通道容量 256           | 有界缓冲防止内存泄漏；丢弃事件可接受（UI 会重连）                          |
| AtomicI64 追踪活跃度       | 无锁空闲检测；毫秒精度对清理足够                                           |
| Agent 工厂为闭包           | 允许同步调用方上下文（thread scope + block_on）在构造时执行异步仓储查询    |
