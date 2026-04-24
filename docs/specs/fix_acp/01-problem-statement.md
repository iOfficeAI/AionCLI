# ACP 协议集成：问题陈述

> **范围**: `crates/aionui-ai-agent`（仅 ACP agent 通信路径）
> **状态**: Draft

---

## 1. 什么是 ACP

ACP (Agent Client Protocol) 是基于 JSON-RPC 2.0 的 AI coding agent 通信协议
（Claude, Codex, Qwen 等）。官方 Rust SDK 为 `agent-client-protocol`（crate 版本 0.11.1，由 Zed 维护）。

SDK 提供：

- `Client` / `Agent` 角色类型及 builder API
- `ByteStreams` — 基于 `AsyncRead + AsyncWrite` 的 NDJSON 传输层
- 类型化的请求/响应结构体（`NewSessionRequest`, `PromptRequest` 等）
- JSON-RPC 路由、消息校验、请求/响应 ID 自动匹配
- 结构化 `Error`，含 `ErrorCode` 枚举（标准 JSON-RPC 错误码）
- `ConnectionTo<Agent>` — 用于发送请求和接收回调的连接句柄

## 2. 现状

`AcpAgentManager`（`acp_agent.rs`）通过手动构造 `{ "type": "<command>", "data": {…} }`
JSON 并逐行写入 stdin 与 CLI 子进程通信。响应从 stdout 按行读取 JSON，通过
serde tagged enum 反序列化为 `AgentStreamEvent`。

这**不是** ACP 协议。它是一种自定义的临时格式，之所以能与部分后端工作，是因为那些
后端同时支持这种非 ACP 兼容模式的输出。

### 存在的问题

| # | 问题 | 影响 |
|---|------|------|
| 1 | **缺少 JSON-RPC** — 消息没有 `jsonrpc`、`id`、`method` 字段 | 无法区分 request 和 notification；没有请求/响应关联 |
| 2 | **缺少握手** — 启动时没有 `initialize` 交换 | 客户端不知道服务端能力，反之亦然 |
| 3 | **Fire-and-forget** — `process.send()` 在字节写入 stdin 后就返回 `Ok(())` | 调用方永远不知道命令是否成功、失败、还是根本没被理解 |
| 4 | **缺少结构化错误** — 所有失败都是 `AppError::Internal(String)` | 无法区分可重试和永久错误；无法映射到正确的 HTTP 状态码 |
| 5 | **缺少断连检测** — 进程在 prompt 中途崩溃时，event relay 循环静默结束 | 进行中的操作挂起或静默消失；没有崩溃诊断信息（无 stderr、无 exit code、无 signal） |
| 6 | **缺少 Agent→Client RPC** — ACP 定义 `requestPermission`、`readTextFile`、`writeTextFile` 为 agent 发向 client 的 *request*（需要 response） | 当前代码把权限请求当作单向事件；agent 永远收不到 JSON-RPC response |

### 不需要改的部分

以下设计良好，应当保留：

- `CliAgentProcess` — 可靠的进程生命周期管理（spawn, kill, graceful shutdown）
- `IAgentManager` trait — 干净的抽象边界
- `AgentStreamEvent` enum — 面向前端的明确契约
- Event relay 模式 — broadcast channel 实现 fan-out
- `AcpState` — 精简、聚焦的运行时状态
- 测试覆盖 — 状态转换和事件解析有良好的单元测试

## 3. 目标

用 SDK 实现的标准 ACP 协议替代临时的 `{ type, data }` 格式，同时保留上述已有设计。

### 非目标

- 修改 `IAgentManager` trait 签名
- 修改 `AgentStreamEvent` enum（前端契约）
- 修改路由 handler 或 API 类型
- 从头重写 `CliAgentProcess`
- 添加自动重连 / auto-resume 逻辑（未来工作）
- 支持 ACP 的 WebSocket 传输（未来工作）

## 4. 验收标准

1. `AcpAgentManager` 通过 ACP JSON-RPC 2.0 通信（由 SDK 保证）
2. 任何 session 操作前完成 `initialize` 握手
3. `prompt()` 返回类型化的 `PromptResponse`（不再是 fire-and-forget）
4. Agent→Client 的 `requestPermission` 作为 JSON-RPC request 处理，
   并返回正确的 response
5. 进程崩溃产生结构化的 `AcpError`，包含 exit code、signal、stderr
6. 现有 `acp_agent.rs` 的所有单元测试继续通过（测试 setup 可能适配，
   但断言必须保持同等具体）
7. `cargo clippy --workspace -- -D warnings` 通过
8. `cargo fmt --all -- --check` 通过
