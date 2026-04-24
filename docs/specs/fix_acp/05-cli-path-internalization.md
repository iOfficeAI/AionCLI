# ACP Agent 创建流程修正：Agent 信息内部化 + Workspace 兜底

> **范围**: `aionui-ai-agent`（types / acp_agent / acp_service）
> **性质**: Bug fix + 架构修正（前端迁移到 Rust HTTP 后端时的遗漏）

---

## 1. 问题

前端（`feat/backend-migration`）创建 ACP 会话后发送消息，Rust HTTP 后端返回错误。
UI 显示 "Failed to send message. Please try again."

### 根因

数据库中新建的 ACP 会话 `extra` 字段：

```json
{
  "agent_name": "Claude",
  "backend": "claude",
  "custom_workspace": false,
  "workspace": ""
}
```

Rust HTTP 后端的 `AcpBuildExtra` 要求 `cli_path: String`（必填），但 `extra` 里没有这个字段。
`serde_json::from_value::<AcpBuildExtra>(extra)` 反序列化失败。

### 为什么 `cli_path` 缺失

| 步骤 | 旧架构（Electron 进程） | 新架构（Rust HTTP 后端） |
|------|------------------------|-------------------|
| Agent 检测 | `AcpDetector` 返回 `AcpDetectedAgent`，包含 `cli_path` | `GET /api/acp/agents` 返回 `AcpAgentInfo`，**无 `cli_path` 字段** |
| 前端创建会话 | `buildAgentConversationParams` 把 `cli_path` 写入 `extra` | `cli_path` 为 undefined，跳过 |
| Rust HTTP 后端 spawn agent | 从 `extra.cli_path` 取路径 | 字段缺失，反序列化失败 |

### 为什么 `workspace` 为空

| 步骤 | 旧架构 | 新架构 |
|------|--------|--------|
| 用户未选目录 | Electron 进程的 `buildWorkspaceWithFiles()` 创建临时目录 | `POST /api/conversations` 原样存 `extra`，无兜底 |

---

## 2. 设计决策

### `cli_path` 和 `backend` 都不应由前端传入

旧架构中前端拿到 `cli_path` 后原样传回 Rust HTTP 后端，这是不合理的循环。
`backend` 字段（`"claude"`, `"codex"` 等）本质上就是 agent 的标识，与 `id` 重复。

正确的职责划分：

- 前端从 `GET /api/acp/agents` 获取 agent 列表（含 `id`）
- 前端创建会话时只传 **agent `id`**（例如 `"claude"`）
- Rust HTTP 后端通过 id 查到 `AcpBackend` 枚举 → `cli_binary_name()` → `which::which()` → CLI 路径
- CLI 路径、spawn 参数等**全部是 Rust HTTP 后端内部实现，不暴露给前端**

### `AcpBuildExtra` 瘦身

从 `AcpBuildExtra` 中移除：
- `cli_path` — Rust HTTP 后端自行解析
- `backend` — 由 `id` 代替。Rust HTTP 后端通过 `id` → `AcpBackend` 映射

保留（前端仍需传入的）：
- `id` — agent 标识（新增，取代 `backend`）
- `workspace` — 用户指定的工作目录（可选，空则兜底）
- `custom_workspace` — 用户是否主动选择了工作目录
- `agent_name` — agent 显示名
- `custom_agent_id` — 自定义 agent ID
- `preset_context`、`enabled_skills`、`session_mode` 等业务配置

### Workspace 兜底

前端不传 workspace（或传空字符串）时，Rust HTTP 后端创建临时工作目录。
与旧架构行为一致。

---

## 3. 变更清单

### 3.1 `AcpBuildExtra`（`crates/aionui-ai-agent/src/types.rs`）

```rust
pub struct AcpBuildExtra {
    /// Agent 标识（如 "claude", "codex", "qwen"）。
    /// Rust HTTP 后端通过此 ID 查找 AcpBackend 枚举和 CLI 路径。
    pub id: String,
    // backend: 移除，由 id 映射得到。
    // cli_path: 移除，Rust HTTP 后端自行解析。
    #[serde(default)]
    pub custom_workspace: bool,
    #[serde(default)]
    pub agent_name: Option<String>,
    // ... 其余 Optional 字段不变
}
```

### 3.2 `acp_service.rs` — 新增 `resolve_backend()` 和 `resolve_cli_path()`

```rust
/// 通过 agent id 解析 AcpBackend 枚举。
pub fn resolve_backend(agent_id: &str) -> Result<AcpBackend, AppError> {
    // 从 known_agents() 中查找，或直接 serde 反序列化
    serde_json::from_value(serde_json::Value::String(agent_id.to_owned()))
        .map_err(|_| AppError::BadRequest(format!("Unknown ACP agent: {agent_id}")))
}

/// 通过 AcpBackend 解析 CLI 可执行文件路径。
pub fn resolve_cli_path(backend: AcpBackend) -> Result<String, AppError> {
    let binary = cli_binary_name(backend).ok_or_else(|| {
        AppError::BadRequest(format!("Backend {backend:?} has no CLI binary"))
    })?;
    which::which(binary)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| AppError::BadRequest(format!("CLI '{binary}' not found in PATH")))
}
```

### 3.3 `AcpAgentManager::new()`（`crates/aionui-ai-agent/src/acp_agent.rs`）

```rust
pub async fn new(
    conversation_id: String,
    workspace: String,
    config: AcpBuildExtra,
) -> Result<Self, AppError> {
    // 1. 通过 id 解析 backend 和 CLI 路径
    let backend = acp_service::resolve_backend(&config.id)?;
    let cli_path = acp_service::resolve_cli_path(backend)?;

    // 2. Workspace 兜底：空则创建临时目录
    let workspace = if workspace.is_empty() {
        let dir = std::env::temp_dir()
            .join(format!("{}-temp-{}", config.id, aionui_common::now_ms()));
        std::fs::create_dir_all(&dir).map_err(|e| {
            AppError::Internal(format!("Failed to create temp workspace: {e}"))
        })?;
        dir.to_string_lossy().into_owned()
    } else {
        workspace
    };

    let spawn_config = Self::build_spawn_config(&cli_path, &workspace, &config);
    // ...
}
```

### 3.4 内部使用 `backend` 的地方

`AcpAgentManager` 内部仍持有 `backend: AcpBackend` 字段（从 `resolve_backend()` 得到），
用于 `SessionResumeStrategy::for_backend()`、`yolo_mode_value()` 等内部逻辑。
这不影响外部接口。

### 3.5 测试更新

- `AcpBuildExtra` 构造：用 `id: "claude".into()` 取代 `backend: AcpBackend::Claude` + `cli_path: "..."` 
- 新增 `resolve_backend()` 和 `resolve_cli_path()` 单元测试
- 新增 workspace 兜底测试

---

## 4. 不变的部分

| 组件 | 状态 |
|------|------|
| `AcpAgentInfo` API 响应结构 | 不变（`id`, `name`, `backend`, `available`） |
| `GET /api/acp/agents` | 不变 |
| `GET /api/acp/detect-cli` | 保留 |
| `IAgentManager` trait | 不变 |
| `AgentStreamEvent` | 不变 |
| 前端 `buildAgentConversationParams` | 只需确保传 `id`（目前传的 `backend` 值和 `id` 相同） |

---

## 5. 向后兼容

旧版 `extra` 可能包含 `cli_path`/`cliPath` 和 `backend` 字段。
移除这些字段后：

- `cli_path`/`cliPath`：serde 默认忽略未知字段，不影响反序列化
- `backend`：需要兼容 — 如果 `extra` 中有 `backend` 但没有 `id`，
  反序列化会失败。解决方案：用 `#[serde(alias = "backend")]` 让 `id` 字段
  同时接受 `"id"` 和 `"backend"` 两个 key

```rust
pub struct AcpBuildExtra {
    #[serde(alias = "backend")]
    pub id: String,
    // ...
}
```

这样旧数据 `{"backend":"claude",...}` 和新数据 `{"id":"claude",...}` 都能正确解析。
