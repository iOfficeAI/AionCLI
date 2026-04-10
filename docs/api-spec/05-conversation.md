# 05 - 会话与消息管理

## 概述

管理 AI 对话会话的完整生命周期：创建、消息发送与流式响应、工具调用确认、会话状态跟踪、消息搜索。支持多种 AI 后端类型（Gemini、ACP、OpenClaw、Nanobot、Remote、Aionrs），通过统一的会话接口屏蔽后端差异。

**源码位置**：`process/bridge/conversationBridge.ts`、`process/bridge/acpConversationBridge.ts`、`process/bridge/geminiConversationBridge.ts`、`process/task/`、`common/chat/`

## 架构设计

### 功能分区

```
会话与消息管理
├── 会话 CRUD              → 创建、查询、更新、删除会话
├── 消息收发               → 发送用户消息、接收 AI 流式响应
├── 确认系统               → 工具调用确认（文件编辑、命令执行等）
├── 审批记忆               → 会话级 "始终允许" 记录
├── 辅助查询               → 不中断主对话的快速提问（仅 ACP Claude）
├── 工作区浏览             → 查看会话关联的文件目录
├── 消息搜索               → 跨会话全文检索
└── ACP 后端管理           → CLI 检测、Agent 健康检查、会话模式/模型切换
```

### 会话类型体系

| 类型 | 说明 | 后端 |
|------|------|------|
| `gemini` | Gemini CLI Agent | Gemini CLI |
| `acp` | ACP 协议 Agent（含多个子后端） | Claude、Qwen、CodeBuddy、OpenCode、Codex、Kiro 等 |
| `openclaw-gateway` | OpenClaw Gateway Agent | OpenClaw CLI |
| `nanobot` | Nanobot Agent | Nanobot |
| `remote` | 远程 Agent | 远程服务器 |
| `aionrs` | Aionrs Agent | Aionrs CLI |

### 消息流转架构

```
客户端                    Rust 后端                     AI 后端
  │                         │                            │
  │── POST /conversations ──→│                            │
  │←── 201 { id, ... } ─────│                            │
  │                         │                            │
  │── POST /send-message ──→│── 启动 Agent Task ─────────→│
  │←── 202 Accepted ────────│                            │
  │                         │←── 流式响应 ────────────────│
  │←── WS: message.stream ──│                            │
  │←── WS: message.stream ──│                            │
  │                         │                            │
  │                         │←── 工具调用请求 ────────────│
  │←── WS: confirmation.add │                            │
  │── POST /confirm ────────→│── 确认结果 ─────────────────→│
  │                         │←── 继续流式响应 ────────────│
  │←── WS: message.stream ──│                            │
  │                         │                            │
  │←── WS: turn.completed ──│                            │
```

## REST API

### POST /api/conversations

创建新会话。

**需要认证**：是

**请求体**：

```json
{
  "type": "gemini",
  "name": "代码审查",
  "model": {
    "providerId": "uuid-xxx",
    "model": "claude-sonnet-4-20250514"
  },
  "extra": {
    "workspace": "/path/to/project",
    "presetRules": "你是代码审查助手"
  }
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `type` | `AgentType` | 是 | 会话类型 |
| `name` | `string` | 否 | 显示名称（不传则自动生成） |
| `model` | `ProviderWithModel` | 是 | 模型配置 |
| `source` | `ConversationSource` | 否 | 来源，默认 `aionui` |
| `channelChatId` | `string` | 否 | 通道隔离 ID（通道来源时必填） |
| `extra` | `object` | 是 | 类型特定参数（见下方各类型详情） |

**各类型 `extra` 字段**：

<details>
<summary>Gemini Extra</summary>

| 字段 | 类型 | 说明 |
|------|------|------|
| `workspace` | `string` | 工作目录路径 |
| `customWorkspace` | `boolean` | 是否用户自定义路径 |
| `webSearchEngine` | `"google" \| "default"` | 网页搜索引擎 |
| `contextFileName` | `string` | 上下文文件名 |
| `contextContent` | `string` | 上下文内容 |
| `presetRules` | `string` | 预设规则 |
| `enabledSkills` | `string[]` | 启用的技能 |
| `extraSkillPaths` | `string[]` | 额外技能路径 |
| `excludeBuiltinSkills` | `string[]` | 排除的内置技能 |
| `presetAssistantId` | `string` | 预设助手 ID |
| `sessionMode` | `string` | 会话模式 |
| `cronJobId` | `string` | 关联的定时任务 ID |

</details>

<details>
<summary>ACP Extra</summary>

| 字段 | 类型 | 说明 |
|------|------|------|
| `workspace` | `string` | 工作目录路径 |
| `backend` | `AcpBackend` | 子后端标识 |
| `cliPath` | `string` | CLI 可执行文件路径 |
| `customWorkspace` | `boolean` | 是否用户自定义路径 |
| `agentName` | `string` | Agent 名称 |
| `customAgentId` | `string` | 自定义 Agent ID |
| `presetContext` | `string` | 预设上下文 |
| `enabledSkills` | `string[]` | 启用的技能 |
| `presetAssistantId` | `string` | 预设助手 ID |
| `sessionMode` | `string` | 会话模式 |
| `cronJobId` | `string` | 关联的定时任务 ID |

</details>

<details>
<summary>OpenClaw Extra</summary>

| 字段 | 类型 | 说明 |
|------|------|------|
| `workspace` | `string` | 工作目录路径 |
| `backend` | `AcpBackend` | 子后端标识 |
| `agentName` | `string` | Agent 名称 |
| `customWorkspace` | `boolean` | 是否用户自定义路径 |
| `gateway` | `OpenClawGatewayConfig` | 网关配置 |
| `enabledSkills` | `string[]` | 启用的技能 |
| `presetAssistantId` | `string` | 预设助手 ID |
| `cronJobId` | `string` | 关联的定时任务 ID |

</details>

**成功响应** `201`：

```json
{
  "success": true,
  "data": {
    "id": "conv-uuid-xxx",
    "name": "代码审查",
    "type": "gemini",
    "model": {
      "providerId": "uuid-xxx",
      "model": "claude-sonnet-4-20250514"
    },
    "status": "pending",
    "source": "aionui",
    "createdAt": 1712345678000,
    "modifiedAt": 1712345678000,
    "extra": { "workspace": "/path/to/project" }
  }
}
```

**副作用**：
- 通过 WebSocket 广播 `conversation.listChanged` 事件（`action: "created"`）

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | 类型不合法、必填字段缺失、模型配置无效 |
| 403 | 未认证 |
| 500 | 服务器内部错误 |

---

### GET /api/conversations

获取会话列表（分页）。

**需要认证**：是

**查询参数**：

| 参数 | 类型 | 说明 |
|------|------|------|
| `cursor` | `string` | 分页游标（上一页最后一个会话 ID） |
| `limit` | `number` | 每页数量，默认 20 |
| `source` | `string` | 按来源筛选 |
| `cronJobId` | `string` | 按关联定时任务筛选 |
| `pinned` | `boolean` | 按置顶状态筛选 |

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "items": [
      {
        "id": "conv-uuid-xxx",
        "name": "代码审查",
        "type": "gemini",
        "model": { "providerId": "...", "model": "..." },
        "status": "finished",
        "source": "aionui",
        "pinned": false,
        "createdAt": 1712345678000,
        "modifiedAt": 1712345680000,
        "extra": { "workspace": "/path/to/project" }
      }
    ],
    "hasMore": true,
    "total": 42
  }
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 500 | 服务器内部错误 |

---

### GET /api/conversations/:id

获取单个会话详情。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true,
  "data": { /* 完整会话对象 */ }
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### PATCH /api/conversations/:id

更新会话信息。

**需要认证**：是

**请求体**：

```json
{
  "name": "新名称",
  "pinned": true,
  "extra": {
    "workspace": "/new/path"
  }
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `name` | `string` | 显示名称 |
| `pinned` | `boolean` | 置顶状态 |
| `model` | `ProviderWithModel` | 模型配置 |
| `extra` | `object` | 类型特定参数（合并更新） |

> **设计决策**：`extra` 字段采用合并更新（merge）而非替换。原实现中 `mergeExtra` 参数控制此行为，Rust 重写时默认合并，简化接口。

**成功响应** `200`：

```json
{
  "success": true,
  "data": { /* 更新后的完整会话对象 */ }
}
```

**副作用**：
- 通过 WebSocket 广播 `conversation.listChanged` 事件（`action: "updated"`）

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | 字段校验失败 |
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### DELETE /api/conversations/:id

删除会话。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true
}
```

**副作用**：
- 终止正在运行的 Agent 任务
- 清理通道关联数据（如非 aionui 来源）
- 通过 WebSocket 广播 `conversation.listChanged` 事件（`action: "deleted"`）

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### POST /api/conversations/:id/messages

发送用户消息。

**需要认证**：是

**请求体**：

```json
{
  "content": "请审查这段代码的安全性",
  "msgId": "msg-uuid-xxx",
  "files": ["/path/to/file.ts"],
  "injectSkills": ["security-review"]
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `content` | `string` | 是 | 用户消息内容 |
| `msgId` | `string` | 是 | 客户端生成的消息 ID |
| `files` | `string[]` | 否 | 附带文件路径列表 |
| `injectSkills` | `string[]` | 否 | 注入的技能列表 |

**成功响应** `202`：

```json
{
  "success": true
}
```

> **设计决策**：返回 `202 Accepted` 而非 `200`，因为消息处理是异步的。AI 响应通过 WebSocket 流式推送。

**副作用**：
- 创建或复用 Agent 任务
- Gemini 类型：将附带文件复制到工作区目录
- AI 响应通过 WebSocket `message.stream` 事件流式推送
- 完成后通过 WebSocket 推送 `turn.completed` 事件

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | 消息内容为空 |
| 403 | 未认证 |
| 404 | 会话不存在 |
| 409 | 会话正在处理中（且未启用命令队列） |
| 500 | 服务器内部错误 |

---

### POST /api/conversations/:id/stop

停止当前流式响应。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true
}
```

**副作用**：
- 终止 Agent 的当前处理流程
- 通过 WebSocket 推送 `turn.completed` 事件（`status: "finished"`）

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 409 | 会话未在运行中 |
| 500 | 服务器内部错误 |

---

### POST /api/conversations/:id/reset

重置会话（清除历史，保留配置）。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true
}
```

**副作用**：
- 终止正在运行的 Agent 任务
- 清除会话消息历史
- 重置状态为 `pending`

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### POST /api/conversations/:id/warmup

预热会话（提前初始化 Agent 任务，加速首次消息响应）。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### GET /api/conversations/:id/messages

获取会话消息列表（分页）。

**需要认证**：是

**查询参数**：

| 参数 | 类型 | 说明 |
|------|------|------|
| `page` | `number` | 页码，默认 1 |
| `pageSize` | `number` | 每页数量，默认 50 |
| `order` | `"ASC" \| "DESC"` | 排序方向，默认 `DESC` |

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "items": [
      {
        "id": "msg-uuid-xxx",
        "conversationId": "conv-uuid-xxx",
        "type": "text",
        "content": { "content": "请审查这段代码" },
        "position": "right",
        "status": "finish",
        "createdAt": 1712345678000
      },
      {
        "id": "msg-uuid-yyy",
        "conversationId": "conv-uuid-xxx",
        "type": "text",
        "content": { "content": "我来审查一下..." },
        "position": "left",
        "status": "finish",
        "createdAt": 1712345679000
      }
    ],
    "total": 128,
    "hasMore": true
  }
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### GET /api/messages/search

跨会话搜索消息。

**需要认证**：是

**查询参数**：

| 参数 | 类型 | 说明 |
|------|------|------|
| `keyword` | `string` | 搜索关键词（必填） |
| `page` | `number` | 页码，默认 1 |
| `pageSize` | `number` | 每页数量，默认 20 |

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "items": [
      {
        "messageId": "msg-uuid-xxx",
        "conversationId": "conv-uuid-xxx",
        "conversationName": "代码审查",
        "type": "text",
        "content": "...匹配片段...",
        "createdAt": 1712345678000
      }
    ],
    "total": 5,
    "hasMore": false
  }
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | keyword 为空 |
| 403 | 未认证 |
| 500 | 服务器内部错误 |

---

### POST /api/conversations/:id/confirmations/:callId/confirm

确认工具调用。

**需要认证**：是

**请求体**：

```json
{
  "msgId": "msg-uuid-xxx",
  "data": { "label": "允许", "value": "allow" },
  "alwaysAllow": false
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `msgId` | `string` | 是 | 消息 ID |
| `data` | `any` | 是 | 用户选择的确认选项值 |
| `alwaysAllow` | `boolean` | 否 | 是否记住此选择（会话级） |

**成功响应** `200`：

```json
{
  "success": true
}
```

**副作用**：
- Agent 收到确认结果后恢复执行
- 通过 WebSocket 推送 `confirmation.remove` 事件
- 若 `alwaysAllow=true`，后续同类操作自动批准

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | 参数无效 |
| 403 | 未认证 |
| 404 | 会话或确认项不存在 |
| 500 | 服务器内部错误 |

---

### GET /api/conversations/:id/confirmations

获取会话的待确认列表。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true,
  "data": [
    {
      "id": "confirm-uuid-xxx",
      "callId": "call-123",
      "title": "文件编辑确认",
      "action": "edit_file",
      "description": "编辑 src/main.rs",
      "commandType": null,
      "options": [
        { "label": "允许", "value": "allow" },
        { "label": "拒绝", "value": "deny" },
        { "label": "始终允许", "value": "always_allow" }
      ]
    }
  ]
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### GET /api/conversations/:id/approvals/check

检查操作是否已被自动批准。

**需要认证**：是

**查询参数**：

| 参数 | 类型 | 说明 |
|------|------|------|
| `action` | `string` | 操作类型（必填） |
| `commandType` | `string` | 命令类型（可选，如 `curl`、`npm`） |

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "approved": true
  }
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | action 为空 |
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### POST /api/conversations/:id/side-question

在不中断主对话的情况下快速提问（仅支持 ACP Claude 后端）。

**需要认证**：是

**请求体**：

```json
{
  "question": "这个函数的作用是什么？"
}
```

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "status": "ok",
    "answer": "这个函数用于..."
  }
}
```

**可能的 status 值**：

| status | 说明 |
|--------|------|
| `ok` | 成功获取答案 |
| `unsupported` | 当前后端不支持辅助查询 |
| `invalid` | 问题无效（如为空） |
| `toolsRequired` | 回答需要使用工具，无法简单回答 |
| `noAnswer` | 超时（30 秒）或无法回答 |

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | 问题为空 |
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### GET /api/conversations/:id/workspace

浏览会话关联的工作区文件目录。

**需要认证**：是

**查询参数**：

| 参数 | 类型 | 说明 |
|------|------|------|
| `path` | `string` | 目录路径（必填，相对于工作区根目录） |
| `search` | `string` | 文件名搜索关键词 |

**成功响应** `200`：

```json
{
  "success": true,
  "data": [
    { "name": "src", "type": "directory" },
    { "name": "Cargo.toml", "type": "file" },
    { "name": "README.md", "type": "file" }
  ]
}
```

**限制**：
- 最大目录遍历深度：10 层

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | path 为空 |
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### POST /api/conversations/:id/reload-context

重新加载会话上下文（刷新技能、工作区等）。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### GET /api/conversations/:id/associated

获取关联会话列表（如同一工作区的其他会话）。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true,
  "data": [
    { "id": "conv-uuid-yyy", "name": "另一个会话", "type": "gemini" }
  ]
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### GET /api/conversations/:id/slash-commands

获取会话可用的斜杠命令列表。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true,
  "data": [
    {
      "command": "/review",
      "description": "代码审查"
    },
    {
      "command": "/test",
      "description": "运行测试"
    }
  ]
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 403 | 未认证 |
| 404 | 会话不存在 |
| 500 | 服务器内部错误 |

---

### POST /api/conversations/clone

从现有会话创建新会话（克隆配置，不复制消息）。

**需要认证**：是

**请求体**：

```json
{
  "sourceConversationId": "conv-uuid-xxx",
  "conversation": {
    "type": "gemini",
    "name": "克隆会话",
    "model": { "providerId": "...", "model": "..." },
    "extra": {}
  },
  "migrateCron": false
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `sourceConversationId` | `string` | 否 | 源会话 ID（复制配置） |
| `conversation` | `object` | 是 | 新会话参数（同创建接口） |
| `migrateCron` | `boolean` | 否 | 是否迁移定时任务绑定 |

**成功响应** `201`：

```json
{
  "success": true,
  "data": { /* 新会话对象 */ }
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | 参数无效 |
| 403 | 未认证 |
| 404 | 源会话不存在 |
| 500 | 服务器内部错误 |

## ACP 后端管理 API

### POST /api/acp/detect-cli

检测 ACP CLI 可执行文件路径。

**需要认证**：是

**请求体**：

```json
{
  "backend": "claude"
}
```

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "path": "/usr/local/bin/claude"
  }
}
```

**错误响应**：

| 状态码 | 场景 |
|--------|------|
| 400 | backend 无效 |
| 403 | 未认证 |
| 500 | 服务器内部错误 |

---

### GET /api/acp/agents

获取可用的 ACP Agent 列表。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true,
  "data": [
    {
      "id": "agent-1",
      "name": "Claude",
      "backend": "claude",
      "available": true
    }
  ]
}
```

---

### POST /api/acp/agents/refresh

刷新自定义 Agent 列表。

**需要认证**：是

---

### POST /api/acp/agents/test

测试自定义 Agent 连接。

**需要认证**：是

**请求体**：

```json
{
  "command": "/path/to/agent",
  "acpArgs": ["--flag"],
  "env": { "KEY": "value" }
}
```

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "step": "completed"
  }
}
```

---

### POST /api/acp/health-check

检查 ACP 后端健康状态。

**需要认证**：是

**请求体**：

```json
{
  "backend": "claude"
}
```

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "available": true,
    "latency": 120,
    "error": null
  }
}
```

---

### GET /api/acp/env

获取 ACP 环境变量信息。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "env": {
      "PATH": "...",
      "HOME": "..."
    }
  }
}
```

---

### GET /api/conversations/:id/acp/mode

获取 ACP 会话模式。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "mode": "code",
    "initialized": true
  }
}
```

---

### PUT /api/conversations/:id/acp/mode

设置 ACP 会话模式。

**需要认证**：是

**请求体**：

```json
{
  "mode": "code"
}
```

---

### GET /api/conversations/:id/acp/model

获取 ACP 会话的模型信息。

**需要认证**：是

---

### PUT /api/conversations/:id/acp/model

切换 ACP 会话的模型。

**需要认证**：是

**请求体**：

```json
{
  "modelId": "claude-sonnet-4-20250514"
}
```

---

### POST /api/acp/probe-model

探测 ACP 后端可用的模型信息。

**需要认证**：是

**请求体**：

```json
{
  "backend": "claude"
}
```

---

### GET /api/conversations/:id/acp/config

获取 ACP 会话配置选项。

**需要认证**：是

---

### PUT /api/conversations/:id/acp/config/:configId

设置 ACP 会话配置选项。

**需要认证**：是

**请求体**：

```json
{
  "value": "some-value"
}
```

---

### GET /api/conversations/:id/openclaw/runtime

获取 OpenClaw 运行时信息。

**需要认证**：是

**成功响应** `200`：

```json
{
  "success": true,
  "data": {
    "workspace": "/path/to/project",
    "backend": "claude",
    "agentName": "default",
    "cliPath": "/usr/local/bin/openclaw",
    "model": "claude-sonnet-4-20250514",
    "identityHash": "abc123"
  }
}
```

## IPC 接口（Electron → 后端）

> 以下 IPC 接口列出原实现中的通道，及其在 Rust 重写中的目标协议映射。

### 会话 CRUD

| 通道 | 目标协议 | 说明 |
|------|---------|------|
| `conversation.create-conversation` | HTTP `POST /api/conversations` | 创建会话 |
| `conversation.get-conversation` | HTTP `GET /api/conversations/:id` | 获取单个会话 |
| `conversation.update-conversation` | HTTP `PATCH /api/conversations/:id` | 更新会话 |
| `conversation.remove-conversation` | HTTP `DELETE /api/conversations/:id` | 删除会话 |
| `conversation.reset-conversation` | HTTP `POST /api/conversations/:id/reset` | 重置会话 |
| `conversation.create-with-conversation` | HTTP `POST /api/conversations/clone` | 克隆创建会话 |
| `conversation.list-by-cron-job` | HTTP `GET /api/conversations?cronJobId=xxx` | 按定时任务筛选 |
| `conversation.get-associated-conversation` | HTTP `GET /api/conversations/:id/associated` | 获取关联会话 |

### 消息收发

| 通道 | 目标协议 | 说明 |
|------|---------|------|
| `conversation.send.message` | HTTP `POST /api/conversations/:id/messages` | 发送用户消息 |
| `conversation.stop.stream` | HTTP `POST /api/conversations/:id/stop` | 停止流式响应 |
| `conversation.response.stream` | WebSocket `message.stream` | AI 流式响应推送 |
| `conversation.turn.completed` | WebSocket `turn.completed` | 对话轮次完成事件 |
| `conversation.warmup` | HTTP `POST /api/conversations/:id/warmup` | 预热会话 |
| `conversation.reload-context` | HTTP `POST /api/conversations/:id/reload-context` | 重载上下文 |
| `conversation.get-slash-commands` | HTTP `GET /api/conversations/:id/slash-commands` | 获取斜杠命令 |
| `conversation.ask-side-question` | HTTP `POST /api/conversations/:id/side-question` | 辅助查询 |

### 确认系统

| 通道 | 目标协议 | 说明 |
|------|---------|------|
| `confirmation.add` | WebSocket `confirmation.add` | 新增待确认项 |
| `confirmation.update` | WebSocket `confirmation.update` | 更新确认项 |
| `confirmation.remove` | WebSocket `confirmation.remove` | 移除确认项 |
| `confirmation.confirm` | HTTP `POST /api/conversations/:id/confirmations/:callId/confirm` | 提交确认 |
| `confirmation.list` | HTTP `GET /api/conversations/:id/confirmations` | 获取待确认列表 |
| `approval.check` | HTTP `GET /api/conversations/:id/approvals/check` | 检查自动批准 |

### Gemini 特有

| 通道 | 目标协议 | 说明 |
|------|---------|------|
| `input.confirm.message` | HTTP `POST /api/conversations/:id/confirmations/:callId/confirm` | Gemini 输入确认（复用通用确认接口） |

### ACP 管理

| 通道 | 目标协议 | 说明 |
|------|---------|------|
| `acp.detect-cli-path` | HTTP `POST /api/acp/detect-cli` | 检测 CLI 路径 |
| `acp.get-available-agents` | HTTP `GET /api/acp/agents` | 获取可用 Agent |
| `acp.check.env` | HTTP `GET /api/acp/env` | 获取环境变量 |
| `acp.refresh-custom-agents` | HTTP `POST /api/acp/agents/refresh` | 刷新自定义 Agent |
| `acp.test-custom-agent` | HTTP `POST /api/acp/agents/test` | 测试自定义 Agent |
| `acp.check-agent-health` | HTTP `POST /api/acp/health-check` | 健康检查 |
| `acp.get-mode` | HTTP `GET /api/conversations/:id/acp/mode` | 获取会话模式 |
| `acp.set-mode` | HTTP `PUT /api/conversations/:id/acp/mode` | 设置会话模式 |
| `acp.get-model-info` | HTTP `GET /api/conversations/:id/acp/model` | 获取模型信息 |
| `acp.probe-model-info` | HTTP `POST /api/acp/probe-model` | 探测模型信息 |
| `acp.set-model` | HTTP `PUT /api/conversations/:id/acp/model` | 切换模型 |
| `acp.get-config-options` | HTTP `GET /api/conversations/:id/acp/config` | 获取配置选项 |
| `acp.set-config-option` | HTTP `PUT /api/conversations/:id/acp/config/:configId` | 设置配置选项 |

### OpenClaw 特有

| 通道 | 目标协议 | 说明 |
|------|---------|------|
| `openclaw.response.stream` | WebSocket `message.stream` | OpenClaw 流式响应（复用通用流式通道） |
| `openclaw.get-runtime` | HTTP `GET /api/conversations/:id/openclaw/runtime` | 获取运行时信息 |

### 工作区

| 通道 | 目标协议 | 说明 |
|------|---------|------|
| `conversation.get-workspace` | HTTP `GET /api/conversations/:id/workspace` | 浏览工作区文件 |

### 列表变更通知

| 通道 | 目标协议 | 说明 |
|------|---------|------|
| `conversation.list-changed` | WebSocket `conversation.listChanged` | 会话列表变更广播 |

## WebSocket 事件

### message.stream

AI 响应流式推送。

**方向**：服务端 → 客户端

**负载**：

```json
{
  "event": "message.stream",
  "data": {
    "conversationId": "conv-uuid-xxx",
    "msgId": "msg-uuid-xxx",
    "type": "text",
    "data": { "content": "代码中存在..." },
    "hidden": false
  }
}
```

**消息类型**（`type` 字段）：

| type | data 内容 | 说明 |
|------|----------|------|
| `text` | `{ content }` | 文本内容（增量） |
| `tips` | `{ content, type: "error"\|"success"\|"warning" }` | 提示信息 |
| `tool_call` | `{ callId, name, args, status }` | 单个工具调用 |
| `tool_group` | `Array<{ callId, description, name, status, ... }>` | 工具调用组 |
| `agent_status` | `{ backend, status, agentName, sessionId, ... }` | Agent 状态变更 |
| `acp_permission` | ACP 权限请求详情 | ACP 权限确认请求 |
| `acp_tool_call` | 工具调用更新 | ACP 工具调用进度 |
| `codex_permission` | Codex 权限请求详情 | Codex 权限确认请求 |
| `codex_tool_call` | 工具调用更新 | Codex 工具调用进度 |
| `plan` | `{ sessionId, entries }` | 执行计划 |
| `thinking` | `{ content, subject, duration, status }` | 思考过程 |
| `available_commands` | `{ commands }` | 可用命令列表 |
| `skill_suggest` | `{ cronJobId, name, description, skillContent }` | 技能建议 |
| `cron_trigger` | `{ cronJobId, cronJobName, triggeredAt }` | 定时任务触发 |

---

### turn.completed

对话轮次完成事件。

**方向**：服务端 → 客户端

**负载**：

```json
{
  "event": "turn.completed",
  "data": {
    "conversationId": "conv-uuid-xxx",
    "status": "finished"
  }
}
```

| status | 说明 |
|--------|------|
| `pending` | 等待中 |
| `running` | 执行中 |
| `finished` | 已完成 |

---

### confirmation.add

新增待确认项。

**方向**：服务端 → 客户端

**负载**：

```json
{
  "event": "confirmation.add",
  "data": {
    "conversationId": "conv-uuid-xxx",
    "id": "confirm-uuid-xxx",
    "callId": "call-123",
    "title": "文件编辑确认",
    "action": "edit_file",
    "description": "编辑 src/main.rs",
    "commandType": null,
    "options": [
      { "label": "允许", "value": "allow" },
      { "label": "拒绝", "value": "deny" }
    ]
  }
}
```

---

### confirmation.update

更新已有确认项。

**方向**：服务端 → 客户端

**负载**：同 `confirmation.add`

---

### confirmation.remove

移除确认项（已处理或已超时）。

**方向**：服务端 → 客户端

**负载**：

```json
{
  "event": "confirmation.remove",
  "data": {
    "conversationId": "conv-uuid-xxx",
    "id": "confirm-uuid-xxx"
  }
}
```

---

### conversation.listChanged

会话列表变更广播。

**方向**：服务端 → 客户端

**负载**：

```json
{
  "event": "conversation.listChanged",
  "data": {
    "conversationId": "conv-uuid-xxx",
    "action": "created",
    "source": "aionui"
  }
}
```

| action | 说明 |
|--------|------|
| `created` | 新建会话 |
| `updated` | 更新会话 |
| `deleted` | 删除会话 |

## 数据模型

### TChatConversation

会话核心数据结构（带类型判别的联合类型）：

```
TChatConversation {
  id: string                      // UUID
  name: string                    // 显示名称
  desc: string | null             // 描述
  type: AgentType                 // 会话类型（判别字段）
  model: ProviderWithModel        // 模型配置
  status: ConversationStatus      // 运行时状态
  source: ConversationSource      // 来源
  channel_chat_id: string | null  // 通道隔离 ID
  pinned: boolean                 // 是否置顶
  pinned_at: number | null        // 置顶时间
  created_at: number              // 创建时间 (ms)
  modified_at: number             // 最后修改时间 (ms)
  extra: JSON                     // 类型特定的扩展字段
}
```

### ProviderWithModel

```
ProviderWithModel {
  provider_id: string       // 模型提供商 ID
  model: string             // 模型标识（如 "claude-sonnet-4-20250514"）
  use_model: string | null  // 显示/覆盖模型名
}
```

### TMessage

消息数据结构（带类型判别的联合类型）：

```
TMessage {
  id: string                      // UUID
  msg_id: string | null           // 客户端消息 ID（关联用）
  conversation_id: string         // 所属会话 ID
  type: MessageType               // 消息类型（判别字段）
  content: JSON                   // 类型特定内容
  position: MessagePosition       // 显示位置
  status: MessageStatus           // 消息状态
  hidden: boolean                 // 是否隐藏
  created_at: number              // 创建时间 (ms)
}
```

### IConfirmation

确认项数据结构：

```
IConfirmation {
  id: string                      // UUID
  call_id: string                 // 工具调用 ID
  title: string | null            // 确认标题
  action: string | null           // 操作类型（用于审批记忆）
  description: string             // 操作描述
  command_type: string | null     // 命令类型（用于审批记忆分类）
  options: ConfirmationOption[]   // 可选操作列表
}

ConfirmationOption {
  label: string                   // 显示文本
  value: any                      // 选项值
  params: Map<string, string> | null  // i18n 插值参数
}
```

### OpenClawGatewayConfig

OpenClaw 网关配置：

```
OpenClawGatewayConfig {
  host: string | null
  port: number | null
  token: string | null
  password: string | null
  use_external_gateway: boolean
  cli_path: string | null
}
```

### SlashCommandItem

```
SlashCommandItem {
  command: string          // 命令名（含 / 前缀）
  description: string      // 命令描述
}
```

### IDirOrFile

工作区文件/目录条目：

```
IDirOrFile {
  name: string             // 文件/目录名
  type: "file" | "directory"
}
```

## 枚举类型

### AgentType

```
AgentType = "gemini" | "acp" | "openclaw-gateway" | "nanobot" | "remote" | "aionrs"
```

### AcpBackend

ACP 子后端标识（20+ 种）：

```
AcpBackend = "claude" | "gemini" | "qwen" | "iflow" | "codex" | "codebuddy"
           | "droid" | "goose" | "auggie" | "kimi" | "opencode" | "copilot"
           | "qoder" | "openclaw-gateway" | "vibe" | "nanobot" | "cursor"
           | "kiro" | "remote" | "aionrs" | "custom"
```

预设 Agent 类型（可直接路由到 ACP）：
```
PresetAgentType = "gemini" | "claude" | "codex" | "codebuddy" | "opencode" | "qwen" | "kiro"
```

### ConversationStatus

```
ConversationStatus = "pending" | "running" | "finished"
```

### ConversationSource

```
ConversationSource = "aionui" | "telegram" | "lark" | "dingtalk" | "weixin"
```

### MessageType

```
MessageType = "text" | "tips" | "tool_call" | "tool_group" | "agent_status"
            | "acp_permission" | "acp_tool_call" | "codex_permission" | "codex_tool_call"
            | "plan" | "thinking" | "available_commands" | "skill_suggest" | "cron_trigger"
```

### MessagePosition

```
MessagePosition = "left" | "right" | "center" | "pop"
```

- `right`：用户消息
- `left`：AI 响应
- `center`：系统提示
- `pop`：弹出提示

### MessageStatus

```
MessageStatus = "finish" | "pending" | "error" | "work"
```

## 模块依赖

- **依赖**：
  - `02-database`：会话和消息持久化（会话表、消息表）
  - `03-auth`：API 认证中间件
  - `04-system-settings`：获取模型提供商配置、命令队列设置
  - `06-ai-agent`：Agent 实例管理与 AI 后端连接（`IAgentManager`、`IWorkerTaskManager`）
  - `07-realtime`：WebSocket 事件推送（流式响应、确认、列表变更）
  - `08-file-workspace`：文件复制到工作区（Gemini 类型附件处理）
  - `11-cron`：定时任务关联查询
  - `12-mcp`：MCP 服务管理

- **被依赖**：
  - `09-channel`：通道创建/管理会话
  - `11-cron`：定时任务触发会话
  - `15-pet`：桌面宠物事件监听

## 候选公共类型

| 类型 | 来源 | 说明 |
|------|------|------|
| `AgentType` | conversation types | 会话类型枚举，多模块共用 |
| `AcpBackend` | ACP config | ACP 子后端标识 |
| `ConversationSource` | conversation | 会话来源标识 |
| `ProviderWithModel` | conversation / settings | 模型选择配置 |
| `ConversationStatus` | conversation | 运行时状态枚举 |
| `MessageType` | messages | 消息类型枚举 |
| `IConfirmation` | confirmation system | 确认项结构（Agent 模块也需使用） |

## 内部服务接口（Rust Trait 设计参考）

### IConversationService

会话业务逻辑服务：

```
trait IConversationService {
  fn create(&self, params: CreateConversationParams) -> Result<TChatConversation>
  fn get(&self, id: &str) -> Result<Option<TChatConversation>>
  fn update(&self, id: &str, updates: UpdateConversation) -> Result<TChatConversation>
  fn delete(&self, id: &str) -> Result<()>
  fn list(&self, filters: ConversationFilters) -> Result<PaginatedResult<TChatConversation>>
  fn clone_create(&self, params: CloneConversationParams) -> Result<TChatConversation>
}
```

### IConversationRepository

数据访问层：

```
trait IConversationRepository {
  fn get(&self, id: &str) -> Result<Option<TChatConversation>>
  fn create(&self, conversation: &TChatConversation) -> Result<()>
  fn update(&self, id: &str, updates: &UpdateConversation) -> Result<()>
  fn delete(&self, id: &str) -> Result<()>
  fn list_paginated(&self, filters: &ConversationFilters) -> Result<PaginatedResult<TChatConversation>>
  fn list_by_cron_job(&self, cron_job_id: &str) -> Result<Vec<TChatConversation>>
  fn get_messages(&self, id: &str, page: u32, page_size: u32, order: SortOrder) -> Result<PaginatedResult<TMessage>>
  fn insert_message(&self, message: &TMessage) -> Result<()>
  fn search_messages(&self, keyword: &str, page: u32, page_size: u32) -> Result<PaginatedResult<MessageSearchResult>>
}
```

### IWorkerTaskManager

Agent 任务生命周期管理：

```
trait IWorkerTaskManager {
  fn get_task(&self, id: &str) -> Option<Arc<dyn IAgentManager>>
  fn get_or_build_task(&self, id: &str, options: BuildOptions) -> Result<Arc<dyn IAgentManager>>
  fn kill(&self, id: &str, reason: Option<AgentKillReason>) -> Result<()>
  fn clear(&self) -> Result<()>
}
```

### IAgentManager

单个 Agent 实例接口：

```
trait IAgentManager: Send + Sync {
  fn agent_type(&self) -> AgentType
  fn status(&self) -> Option<ConversationStatus>
  fn workspace(&self) -> &str
  fn conversation_id(&self) -> &str
  fn last_activity_at(&self) -> u64

  async fn send_message(&self, data: SendMessageData) -> Result<()>
  async fn stop(&self) -> Result<()>
  fn confirm(&self, msg_id: &str, call_id: &str, data: serde_json::Value) -> Result<()>
  fn get_confirmations(&self) -> Vec<IConfirmation>
  fn kill(&self, reason: Option<AgentKillReason>) -> Result<()>
}
```

## Rust 迁移备注

1. **Agent 进程管理**：使用 `tokio::process::Command` 管理 CLI 子进程（ACP、Gemini、OpenClaw 等），通过 stdin/stdout 通信
2. **流式响应**：使用 `tokio::sync::broadcast` 或 `tokio::sync::mpsc` 将 Agent 输出流转发到 WebSocket
3. **确认系统**：使用 `tokio::sync::oneshot` 实现确认等待/响应模式，Agent 进程阻塞等待确认结果
4. **审批记忆**：会话级内存存储（`HashMap`），随 Agent 任务销毁而清除，不持久化
5. **任务缓存**：`WorkerTaskManager` 用 `DashMap<String, Arc<dyn IAgentManager>>` 管理活跃任务
6. **空闲超时**：ACP 类型 Agent 空闲 30 分钟后自动终止，使用 `tokio::time::interval`（5 分钟检查间隔）
7. **辅助查询**：通过 fork ACP 会话实现，设 30 秒超时，检测到工具调用则立即取消
8. **`extra` 字段**：使用 `serde_json::Value` 存储，按 `type` 反序列化为具体结构。数据库中以 JSON 文本存储
9. **文件附件处理**：Gemini 类型需将文件复制到工作区后传递相对路径；其他类型直接传递缓存目录路径
10. **数据迁移**：首次启动时从旧 JSON 文件（`chat.history`）迁移到 SQLite，后台执行
