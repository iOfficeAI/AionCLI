# 前端接入指南

## TL;DR

> **用户→agent 消息 = 单聊，零差异。**
>
> 每个 team agent 创建时分配了 `conversation_id`，前端用它调通用的单聊接口发消息、拉历史。Team 模块不再提供任何消息端点。

客户端不需要保留任何 team 专属的本地 session、状态复制、wake 逻辑。Team 的调度、状态机、mailbox、任务板全部在后端。全部走 REST + WebSocket 即可。

> **关于 MCP**：agent 之间的通信（发消息、任务板、spawn teammate）走的是 MCP，这是**后端进程 ↔ agent 子进程**之间的事，浏览器前端不接触。详情看 [mcp.md](./mcp.md)。

## 开发进度（实时更新）

| 能力 | 状态 | 说明 |
|------|:---:|------|
| Team CRUD（建/删/改名/加减 agent） | ✅ 已完成 | |
| 用户→agent 发消息（走单聊 API） | ✅ 已完成 | `POST /api/conversations/{conv_id}/messages` |
| Agent 间 MCP 通信（team_send_message 等 10 工具） | ✅ 已完成 | mcp-bridge + TCP server |
| Agent wake 机制（发消息后 agent 自动响应） | ✅ 已完成 | |
| 建团自动起 session（MCP 自动注入） | ✅ 已完成 | `POST /api/teams` 后自动 ensure_session，前端无需额外调用 |
| WS 事件推送（team.agent.status 等） | ✅ 已完成 | |
| user_id 权限隔离（list/get/remove 按用户过滤） | ✅ 已完成 | Wave 3 |
| 单聊→建团（conversation 复用） | ✅ 已完成 | agents 里传 `conversation_id` 可复用 |
| rename 规范化 | ✅ 已完成 | Wave 3 |
| MCP 协议加固（64MB 帧 + 300s 超时） | ✅ 已完成 | Wave 3 |
| Team Guide MCP（agent 自动建团） | ⏳ Wave 5 | `aion_create_team` 工具 |
| team_spawn_agent 真实落地 | ⏳ Wave 5 | leader 动态拉人 |
| crash recovery / 429 识别 | 🔄 Wave 4 进行中 | |

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
| `team.agent.removed` | 移除 agent | `team_id, slot_id` |
| `team.agent.renamed` | 改名 | `team_id, slot_id, name` |

Payload 类型定义在 `crates/aionui-api-types/src/team.rs`。**HTTP 没有状态轮询端点**，想知道 agent 现在在干啥只能靠 WS。

Agent 的回复内容本身走的是 conversation 的 WS 流（`conversation.message.*` / `conversation.stream.*`），与普通单聊完全一致。

## 消息历史：走单聊 API

```
GET /api/conversations/{conversation_id}/messages
```

同样跟单聊一致，不要在 team 路径下找。

## 最小接入 checklist

1. [ ] `POST /api/teams` 建团队，拿到 `team.id` 和每个 `agent.slot_id / conversation_id`（后端自动起 session + 注入 MCP）
2. [ ] 订阅 WS，过滤 `team.agent.*` 事件更新 UI 上的 agent 状态
3. [ ] 进入某 agent 聊天页：`GET /api/conversations/{conversation_id}/messages` 拉历史
4. [ ] 用户在任意 agent 页发言：`POST /api/conversations/{conversation_id}/messages`（lead 也走这个，不再有 team-level 发消息端点）
5. [ ] 重进 app / 后端重启后：调一次 `POST /api/teams/{id}/session`（幂等，重新激活 MCP）
6. [ ] 关闭 team 页/切换：不需要主动 stop session；真要回收调 `DELETE .../session`

**不要做**：不要前端再造一套 agent 状态机/任务调度；不要缓存 mailbox；不要试图通过 team API 拉消息历史或发消息。

## 延伸阅读

- [mcp.md](./mcp.md) — agent 之间通信用的 MCP 协议、工具清单、后端 GAP（前端不直接用，但出现"lead 说 spawn 了但成员数没变"这类现象时需要查这份文档理解原因）
- [internals.md](./internals.md) — 调度器与 mailbox 的细节，查 agent 为什么不响应时用
- [api.md](./api.md) — 全部 REST 端点
