# ACP 协议集成：错误模型

> **位置**: `crates/aionui-ai-agent/src/acp_error.rs`
> **约束**: 必须可转换为 `AppError`（定义在 `aionui-common`）。
> 错误响应不得泄露内部细节（AGENTS.md 安全规则）。

---

## 1. 设计原则

1. **错误是 enum，不是字符串。** 每个变体有固定含义。
2. **可重试性是变体的属性**，不是运行时猜测。
3. **SDK 错误按 code 映射**，不按 message 文本匹配。
4. **进程级错误携带诊断信息**（exit code, signal, stderr）供开发者调试，
   但这些信息**不暴露**在 HTTP 响应中。
5. **`AcpError` → `AppError`** 是离开本 crate 的唯一转换。
   外部调用方只看到 `AppError`，永远不直接接触 `AcpError`。

## 2. `AcpError` 枚举

```rust
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    // ── 进程生命周期 ──────────────────────────────────────────

    /// CLI 二进制文件未找到或不可执行。
    #[error("Failed to spawn agent process: {message}")]
    SpawnFailed {
        message: String,
    },

    /// 进程在 initialize 握手完成前退出。
    #[error("Agent process exited during startup (exit={exit_code:?}, signal={signal:?})")]
    StartupCrash {
        exit_code: Option<i32>,
        signal: Option<String>,
        stderr: String,
    },

    /// 请求进行中时进程崩溃。
    #[error("Agent process disconnected (exit={exit_code:?}, signal={signal:?})")]
    Disconnected {
        exit_code: Option<i32>,
        signal: Option<String>,
        stderr: String,
    },

    // ── ACP 协议错误（来自 SDK ErrorCode）───────────────────

    /// Agent 要求先完成认证。
    #[error("Authentication required")]
    AuthRequired,

    /// Agent 侧找不到该 session ID。
    #[error("Session not found: {session_id}")]
    SessionNotFound {
        session_id: String,
    },

    /// Agent 不支持请求的方法。
    #[error("Method not supported: {method}")]
    MethodNotFound {
        method: String,
    },

    /// 请求参数无效。
    #[error("Invalid parameters: {message}")]
    InvalidParams {
        message: String,
    },

    /// Agent 报告了内部错误。
    #[error("Agent internal error: {message}")]
    AgentInternal {
        message: String,
        code: i32,
    },

    // ── 本地错误 ──────────────────────────────────────────────

    /// 协议未连接（断开后或连接前使用）。
    #[error("ACP protocol not connected")]
    NotConnected,

    /// Initialize 握手超时。
    #[error("Initialize handshake timed out after {timeout_secs}s")]
    InitTimeout {
        timeout_secs: u64,
    },
}
```

## 3. 可重试性

可重试性由变体决定，不作为字段存储：

```rust
impl AcpError {
    /// 调用方是否可以重试该操作。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AcpError::SpawnFailed { .. }
            | AcpError::StartupCrash { .. }
            | AcpError::Disconnected { .. }
            | AcpError::AgentInternal { .. }
            | AcpError::InitTimeout { .. }
        )
    }
}
```

| 变体 | 可重试 | 理由 |
|------|:------:|------|
| `SpawnFailed` | 是 | 二进制可能在安装后出现 |
| `StartupCrash` | 是 | 瞬态崩溃，下次 spawn 可能成功 |
| `Disconnected` | 是 | 进程挂了，重启可能恢复 |
| `AuthRequired` | 是 | 用户可以提供凭据 |
| `SessionNotFound` | 否 | Session 已不存在，需要创建新的 |
| `MethodNotFound` | 否 | Agent 不支持此方法，不会改变 |
| `InvalidParams` | 否 | 调用方 bug |
| `AgentInternal` | 是 | Agent 侧的瞬态故障 |
| `NotConnected` | 否 | 编程错误 — 不应到达用户 |
| `InitTimeout` | 是 | 网络/负载问题，可能恢复 |

## 4. SDK ErrorCode 映射

```rust
impl AcpError {
    /// 将 SDK `Error` 转为 `AcpError`。
    ///
    /// 按 `ErrorCode` 映射，不按 message 文本匹配。
    pub fn from_sdk(err: agent_client_protocol::Error, context: &str) -> Self {
        match err.code {
            ErrorCode::AuthRequired => AcpError::AuthRequired,
            ErrorCode::ResourceNotFound => AcpError::SessionNotFound {
                session_id: context.to_owned(),
            },
            ErrorCode::MethodNotFound => AcpError::MethodNotFound {
                method: context.to_owned(),
            },
            ErrorCode::InvalidParams => AcpError::InvalidParams {
                message: err.message,
            },
            ErrorCode::ParseError
            | ErrorCode::InvalidRequest
            | ErrorCode::InternalError => AcpError::AgentInternal {
                message: err.message,
                code: err.code.into(),
            },
            ErrorCode::Other(code) => match code {
                -32001 | -32002 => AcpError::SessionNotFound {
                    session_id: context.to_owned(),
                },
                _ => AcpError::AgentInternal {
                    message: err.message,
                    code,
                },
            },
        }
    }
}
```

`context` 参数携带当时正在调用的 session ID 或方法名 — 低成本的诊断信息，无需字符串匹配。

## 5. `AcpError` → `AppError` 转换

这是 `AcpError` 离开本 crate 的边界：

```rust
impl From<AcpError> for AppError {
    fn from(err: AcpError) -> Self {
        match &err {
            // 进程生命周期 → 502 Bad Gateway（上游故障）
            AcpError::SpawnFailed { .. }
            | AcpError::StartupCrash { .. }
            | AcpError::Disconnected { .. } => {
                AppError::BadGateway(err.to_string())
            }

            // 认证 → 401
            AcpError::AuthRequired => {
                AppError::Unauthorized("Agent requires authentication".into())
            }

            // Session 未找到 → 404
            AcpError::SessionNotFound { .. } => {
                AppError::NotFound(err.to_string())
            }

            // 方法未找到 → 400
            AcpError::MethodNotFound { .. } => {
                AppError::BadRequest(err.to_string())
            }

            // 参数无效 → 400
            AcpError::InvalidParams { .. } => {
                AppError::BadRequest(err.to_string())
            }

            // Agent 内部错误 → 502（上游故障）
            AcpError::AgentInternal { .. } => {
                AppError::BadGateway(err.to_string())
            }

            // 未连接 → 500（我们的 bug，不是用户的）
            AcpError::NotConnected => {
                AppError::Internal("ACP protocol not connected".into())
            }

            // 初始化超时 → 502
            AcpError::InitTimeout { .. } => {
                AppError::BadGateway(err.to_string())
            }
        }
    }
}
```

**安全说明：** `StartupCrash` 和 `Disconnected` 包含 `stderr`，可能含敏感信息。
`Display` 实现（`thiserror #[error]`）只包含 exit_code 和 signal，不包含 stderr。
stderr 字段可用于结构化日志（tracing），但永远不会序列化到 HTTP 响应中。

## 6. 日志

所有 `AcpError` 在创建点记录完整诊断信息：

```rust
// acp_protocol.rs 中 — 进程崩溃时：
error!(
    conversation_id = %self.conversation_id,
    exit_code = ?info.exit_code,
    signal = ?info.signal,
    stderr = %info.stderr,     // ← 只在日志中，永远不进 HTTP 响应
    "ACP agent process crashed"
);
```

`tracing` 输出包含开发者需要的所有信息。
`AppError` HTTP 响应只包含安全的摘要。

## 7. 与现状对比

| 方面 | 现状 | 改造后 |
|------|------|--------|
| 错误类型 | 所有错误都是 `AppError::Internal(String)` | `AcpError` 枚举，9 个变体 |
| 分类 | 无 | 按 enum 变体 |
| 可重试性 | 未知 | `is_retryable()` 方法 |
| SDK code 映射 | 不适用（无 SDK） | `from_sdk()` 按 `ErrorCode` 映射 |
| HTTP 状态码 | 始终 500 | 400、401、404、502 按变体分配 |
| 诊断信息 | 仅 error message | exit_code + signal + stderr（在日志中） |
| 字符串匹配 | 无（但也没有分类） | 无（按类型分类） |
