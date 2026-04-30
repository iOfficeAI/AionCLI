# 前端接入指南

## TL;DR

> **用户→agent 消息 = 单聊，零差异。**
>
> 每个 team agent 创建时分配了 `conversation_id`，前端用它调通用的单聊接口发消息、拉历史。Team 模块不再提供任何消息端点。

客户端不需要保留任何 team 专属的本地 session、状态复制、wake 逻辑。Team 的调度、状态机、mailbox、任务板全部在后端。全部走 REST + WebSocket 即可。

> **关于 MCP**：agent 之间的通信（发消息、任务板、spawn teammate）走的是 MCP，这是**后端进程 ↔ agent 子进程**之间的事，浏览器前端不接触。详情看 [mcp.md](./mcp.md)。

## 开发进度（实时更新）

> 状态图例：✅ 已完成 / 🔄 进行中 / ⏳ 待做 / ~~SKIPPED~~
> "已完成" = 功能分支已合进 `feat/team-wave4-5`；"进行中" = 代码已写在功能分支但还没合。PR 实际进度以 `git log feat/team-wave4-5 --oneline` 为准。

### 基础能力（Wave 1–3）

| 能力 | 状态 | 说明 |
|------|:---:|------|
| Team CRUD（建/删/改名/加减 agent） | ✅ | |
| 用户→agent 发消息（走单聊 API） | ✅ | `POST /api/conversations/{conv_id}/messages` |
| Agent 间 MCP 通信（team_send_message 等工具） | ✅ | HTTP transport，agent 主动连接 |
| Agent wake 机制（发消息后 agent 自动响应） | ✅ | |
| 建团自动起 session（MCP 自动注入） | ✅ | `POST /api/teams` 后自动 ensure_session，前端无需额外调用 |
| WS 事件推送（team.agent.status 等） | ✅ | |
| user_id 权限隔离（list/get/remove 按用户过滤） | ✅ | Wave 3 |
| 单聊→建团（conversation 复用） | ✅ | agents 里传 `conversation_id` 可复用 |
| rename 规范化 | ✅ | Wave 3 |
| MCP 协议加固（64MB 帧 + 300s 超时） | ✅ | Wave 3 |

### Wave 4 — MCP 传输 + 回合健壮性

| 模块 | 状态 | 说明 |
|------|:---:|------|
| MCP 注入（HTTP transport） | ✅ | commit 6c334a9（替代 stdio bridge） |
| D19a finalize dedup 存储 | ✅ | 5s 去重表 |
| D19b finalize dedup 接入 | ✅ | `on_agent_finish` 已用上 |
| D20a crash 检测 | ✅ | `detect_crash` 纯函数 |
| D20b-1 crash testament | ✅ | 写遗言到 lead mailbox |
| D20b-2 crash handler 编排 | ✅ | `handle_agent_crash`（testament + kill + wake leader） |
| D20c leader-crash 分支 | ✅ | |
| D21 429 限流识别 | ✅ | `is_rate_limited` |
| D22 inactivity watchdog | 🔄 | handler 写完待合（scheduler 有 pending 注释） |
| D23 add_agent 并发锁 | ✅ | |
| D24a MCP ready 协议类型 | ✅ | |
| D24b/c stdio ready 握手 | ~~SKIPPED~~ | HTTP transport 不需要 |
| D25a AgentStreamChunk enum | ✅ | |
| D25b `subscribe_stream()` trait 默认方法 | ✅ | |
| D25c-1 broadcast channel 挂在 AcpAgentManager | ✅ | |
| D25c-2 ACP dispatch 各点 emit | ✅ | `subscribe_stream` 能收到 Text / Thought / ToolUse / Finish / Error |
| D18b-1 wake_timeouts 存储 | ✅ | |
| D18b-2 `arm_wake_timeout` 任务 | ✅ | |
| D18c wake lock 接入 | ✅ | |

### Wave 5 — spawn / shutdown / Guide MCP

| 模块 | 状态 | 说明 |
|------|:---:|------|
| D26a GuideMcpServer 骨架 | ✅ | 应用级单例，暴露 `aion_create_team` / `aion_list_models` 给 solo agent |
| D26b-1 `aion_create_team` 参数解析 | ✅ | |
| D26b-2 `handle_aion_create_team` handler | 🔄 | 调 service + 返回结构化 |
| D26c `aion_list_models` 处理器 | ✅ | |
| D28a `is_team_capable_backend` 白名单 | ✅ | `guide/capability.rs`，白名单 `claude / codex / gemini / aionrs` |
| D28b Guide prompt 注入（solo 互斥） | ✅ | solo agent 首轮消息注入 Team Guide prompt |
| D28c Guide MCP guard（solo 互斥） | ✅ | team 模式下不注入 Guide |
| D29a-1 `SpawnAgentRequest` + 方法骨架 | ✅ | |
| D29a-2 caller role==Lead 校验 | ✅ | |
| D29a-3 name normalize + 唯一性 | ✅ | |
| D29a-4 backend 白名单校验 | ✅ | |
| **D29b spawn_agent 真实落地** | ✅ | **已合入** — conversation 创建 + slot 分配 + kill/warmup agent |
| D29d-1 `team.agent.spawned` WS 事件 | 🔄 | spawn 成功后广播 |
| D29e MCP dispatch 接通 session | 🔄 | `exec_spawn_agent` 改成调 `TeamSession::spawn_agent` |
| D30a-1 shutdown_approved/rejected 字符串拦截 | ✅ | |
| D30a-2 `team.agent.shutdown` WS 事件 | ✅ | shutdown_approved 后广播通知前端 |
| D30b `shutdown_rejected:<reason>` 处理 | ✅ | `mcp/server.rs` 已拦截 |
| D30c `shutdown_agent` target=Lead 校验 | ✅ | 拒绝关 lead |
| D30d-1 `remove_agent` 真 kill agent 进程 | ✅ | |
| D30d-2 `remove_agent` 清 active_wakes / wake_timeouts / finalized_turns | ✅ | |
| D30d-3 `remove_agent` 测试加强 | ✅ | 二次 remove → AgentNotFound + 精确 slot 匹配 |
| D31a TeamMcpPhase enum + WS payload 类型 | ✅ | 10-phase，见下文 |
| D31b-1 `team.mcpStatus` TCP 就绪广播 | ✅ | `TcpReady` / `TcpError` |
| D31b-2 service-layer `team.mcpStatus` 广播 | ✅ | 5 点广播（LoadFailed/SessionError/Injecting/ConfigWriteFailed/Ready） |
| e2e smoke（真实链路） | ✅ | 4 个 scenario 全绿：REST→MCP→Agent→DB |

## MCP Transport 变更（Wave 4）

> **前端影响：无。** 这是后端内部架构变更，对前端接口完全透明。

### 变更内容

从 stdio bridge 切换到 HTTP transport（commit 6c334a9）。

旧方案（已废弃）：后端 spawn stdio bridge 子进程，agent CLI 通过 stdin/stdout 通信 → agent CLI 从未发送 MCP initialize，链路不通。

新方案（当前）：TeamMcpServer 暴露 HTTP 端点，agent CLI 主动通过 HTTP 连接 MCP server → 连接即通，无需额外握手。

### Agent 可用工具

agent 连接 MCP server 后，以下工具对 agent 可见且可调用：

| 工具 | 状态 | 说明 |
|------|:---:|------|
| `team_send_message` | ✅ Working | agent 间发消息；新增 `shutdown_approved` / `shutdown_rejected:<reason>` 字符串语义（Wave 5 D30a/b） |
| `team_spawn_agent` | ✅ Working | caller=Lead 校验 + backend 白名单 + name normalize + 真实创建 conversation + slot 分配 + agent 启动。MCP dispatch 接通进行中 |
| `team_task_create` | ✅ Working | 创建任务 |
| `team_task_update` | ✅ Working | 更新任务状态 |
| `team_task_list` | ✅ Working | 列出所有任务 |
| `team_members` | ✅ Working | 列出当前成员 |
| `team_rename_agent` | ✅ Working | 改名 |
| `team_shutdown_agent` | ✅ Working | Lead 请求 teammate 下线；**已加 target=Lead 校验**（D30c，拒绝关 lead） |

### Team Guide MCP（全局 / Wave 5 新增，落地中）

> 这是 **solo agent**（普通单聊）用来"单聊 → 自动建团"的独立 MCP server，与上面 per-team MCP 不是同一个。

| 工具 | 状态 | 说明 |
|------|:---:|------|
| `aion_create_team` | 🔄 落地中 | 骨架 + 参数解析 ✅，handler (D26b-2) 进行中 |
| `aion_list_models` | ✅ | D26c handler 已合入 |

前端一般不直接感知这个 MCP；但当用户在单聊里说"帮我起一个团队"时，agent 会调 `aion_create_team`，后端应该：
1. 建 team + 复用当前 conversation 作为 lead
2. 启动 session（自动注入 per-team MCP）
3. 推 WS 事件通知前端跳转

D28b/c 已合入：solo team-capable agent 的首轮消息里已注入 Team Guide prompt，且 MCP guard 确保 team 模式下不重复注入。GuideMcpServer（应用级单例）的 HTTP config 注入等全局 server 启动就绪后自动生效。目前 solo agent 可看到 prompt 引导，但 MCP 工具调用还需 Guide server 完全上线。前端仍可走 `POST /api/teams` 显式建团。

### shutdown 协议（Wave 5）

Lead 调 `team_shutdown_agent` 后，teammate 可以在下一个回合里用 `team_send_message` 回复，内容以特定字符串开头：

- `shutdown_approved` → scheduler 触发 `remove_agent`，该 agent 的 `active_wakes` / `wake_timeouts` / `finalized_turns` 被清，最终广播 `team.agent.removed`
- `shutdown_rejected:<reason>` → scheduler 取消 pending 的 shutdown，lead 下一回合能看到理由（已实现）

完整 `remove_agent` 链路已闭环：kill 进程 (D30d-1 ✅) + 清状态 (D30d-2 ✅) + 摘 slot + 广播 `team.agent.removed` (D30d-3 ✅)。shutdown_approved 后还会额外广播 `team.agent.shutdown` (D30a-2 ✅)。

前端不需要特殊处理 — 继续订阅 `team.agent.removed` / `team.agent.status`，收到 `removed` 就把 slot 从列表摘掉。

### 新 WS 事件（Wave 5 D31）

| Event | 何时触发 | Payload 关键字段 |
|-------|---------|----------------|
| `team.mcpStatus` | per-team MCP server 生命周期阶段变化（当前只广播 TCP 层） | `team_id, slot_id (TCP 阶段为空), phase, port?, error?, server_count?` |

`phase` 是 `TeamMcpPhase` 枚举（snake_case）：

```
tcp_binding, tcp_ready, tcp_error,
http_binding, http_ready, http_error,
session_injecting, session_injected,
tools_ready, degraded
```

当前已广播：`tcp_ready`（成功 bind，带 `port`）、`tcp_error`（bind 失败，带 `error`）。其余 phase 随 Wave 5 后续模块（D31b-2 等）陆续接入。

Payload 类型定义：`aionui-api-types::TeamMcpStatusPayload`、`TeamMcpPhase`；另有 `TeammateMessagePayload`（为 teammate 之间消息的左气泡展示预留）。

### 前端须知

- `team_spawn_agent` 当前仍是 no-op：lead 可能"口头"说已创建 agent，前端以 `team.agent.spawned` 事件为准，不要信 lead 的文本
- D24b/c（stdio MCP ready 握手）已跳过 — HTTP transport 不需要
- 已有的 `team.agent.*` 事件格式不变，无需迁移
- `team.mcpStatus` 是**新**事件：前端可以选择订阅用于显示 MCP 连接进度条，忽略也不影响功能

---

### 单聊→建团接入方式（Wave 3 完成后可用）

```json
POST /api/teams
{
  "name": "My Team",
  "agents": [
    {
      "name": "Leader",
      "role": "lead",
      "backend": "claude",
      "model": "claude-sonnet-4",
      "conversation_id": "existing-conv-id"  // ← 传已有单聊的 conv_id，历史消息保留
    },
    {
      "name": "Developer",
      "role": "teammate",
      "backend": "claude",
      "model": "claude-sonnet-4"
      // 不传 conversation_id → 新建
    }
  ]
}
```

传 `conversation_id` 时后端复用该 conversation（extra 打 teamId 标记），不新建。消息历史完整保留。不传则正常新建。

## 必须走 REST 的操作

| 动作 | 端点 |
|------|------|
| 建 team / 加 agent / 改名 / 删 | `/api/teams/**`（见 [api.md](./api.md)） |
| 关闭 session | `DELETE /api/teams/{id}/session` |

> **注意**：`POST /api/teams` 建团后，后端**自动**起 session 并注入 MCP。前端不需要单独调 `POST /api/teams/{id}/session`。但如果后端重启了，需要在进入 team 页时调一次 `POST /api/teams/{id}/session`（幂等）重新激活。

### 发消息：走单聊 API

用户给任何 agent（包括 lead）发消息，统一走：

```
POST /api/conversations/{conversation_id}/messages
```

`conversation_id` 从 `TeamAgentResponse.conversation_id` 取。**跟普通单聊完全一致**，请求体、返回体、WS 事件格式都一样，前端的单聊输入框组件可以直接复用。

Team 模块不再提供 `POST /api/teams/{id}/messages` 或 `POST /api/teams/{id}/agents/{slot_id}/messages`——旧的这两个路由已删除。

## 必须走 WebSocket 的事件

后端通过 `/ws` 推，event name 格式 `team.agent.<action>`：

| Event | 何时触发 | Payload 关键字段 |
|-------|---------|----------------|
| `team.agent.status` | Agent 状态迁移（Idle/Working/...） | `team_id, slot_id, status` |
| `team.agent.spawned` | 新增 agent（REST 或 MCP spawn） | `team_id, agent` |
| `team.agent.removed` | 移除 agent（含 Wave 5 shutdown 链路） | `team_id, slot_id` |
| `team.agent.renamed` | 改名 | `team_id, slot_id, name` |
| `team.mcpStatus` | per-team MCP server 生命周期（Wave 5 新） | `team_id, slot_id?, phase, port?, error?` |

Payload 类型定义在 `crates/aionui-api-types/src/team.rs`（含 `TeamMcpPhase`、`TeamMcpStatusPayload`、`TeammateMessagePayload`）。**HTTP 没有状态轮询端点**，想知道 agent 现在在干啥只能靠 WS。

Agent 的回复内容本身走的是 conversation 的 WS 流（`conversation.message.*` / `conversation.stream.*`），与普通单聊完全一致。

## 消息历史：走单聊 API

```
GET /api/conversations/{conversation_id}/messages
```

同样跟单聊一致，不要在 team 路径下找。

## 最小接入 checklist

1. [ ] `POST /api/teams` 建团队，拿到 `team.id` 和每个 `agent.slot_id / conversation_id`（后端自动起 session + 注入 MCP）
2. [ ] 订阅 WS，过滤 `team.agent.*` 事件更新 UI 上的 agent 状态
3. [ ] （可选）订阅 `team.mcpStatus` 展示 MCP 连接进度，不订阅也能正常用
4. [ ] 进入某 agent 聊天页：`GET /api/conversations/{conversation_id}/messages` 拉历史
5. [ ] 用户在任意 agent 页发言：`POST /api/conversations/{conversation_id}/messages`（lead 也走这个，不再有 team-level 发消息端点）
6. [ ] 重进 app / 后端重启后：调一次 `POST /api/teams/{id}/session`（幂等，重新激活 MCP）
7. [ ] 关闭 team 页/切换：不需要主动 stop session；真要回收调 `DELETE .../session`

**不要做**：不要前端再造一套 agent 状态机/任务调度；不要缓存 mailbox；不要试图通过 team API 拉消息历史或发消息。

## 延伸阅读

- [mcp.md](./mcp.md) — agent 之间通信用的 MCP 协议、工具清单、后端 GAP（前端不直接用，但出现"lead 说 spawn 了但成员数没变"这类现象时需要查这份文档理解原因）
- [internals.md](./internals.md) — 调度器与 mailbox 的细节，查 agent 为什么不响应时用
- [api.md](./api.md) — 全部 REST 端点
