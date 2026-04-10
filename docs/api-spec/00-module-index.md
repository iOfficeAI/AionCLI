# AionUi 后端 - 模块目录索引

## 概述

本文档是 AionUi 后端 API 规范的主索引。目标是从原始 TypeScript/Electron 代码库中梳理所有接口（REST API + IPC），描述其功能语义，为 Rust 重写提供指导。

**源项目**：`../AionUi-Bak`（Electron + TypeScript）
**目标项目**：`aionui-backend`（Rust，Cargo Workspace）

源项目目录结构：

| 目录 | 说明 | 与 Rust 后端的关系 |
|------|------|-------------------|
| `src/process/` | 主进程业务逻辑（bridge、agent、task、services 等） | 主要迁移对象 |
| `src/common/` | 共享代码（类型定义、API 客户端、聊天核心、配置） | 部分迁移（类型、业务逻辑） |
| `src/renderer/` | React 前端 UI | 不迁移 |
| `src/preload/` | Electron preload 脚本 | 不迁移 |
| `src/server/` | 空目录 | 无内容 |

## 梳理方法

- **源码驱动**：逐模块从源码中提取接口定义
- **粒度**：功能语义级（描述"做什么"，而非"怎么实现"）
- **协议映射**：每个 IPC 接口标注目标协议（HTTP / WebSocket / HTTP+WebSocket）
- **公共类型**：梳理过程中标记候选公共类型，所有模块完成后集中提炼到 `01-common-types.md`
- **重设计优于照搬**：梳理目标是提取功能语义，而非复刻原实现的 API 设计。原实现中存在的设计缺陷（命名不一致、格式混用、职责不清等）应在 Rust 重写时修正。梳理时遇到此类问题，在各模块文档中标注为「设计决策」并说明改进方向，前端/客户端随后适配新接口
- **跨会话支持**：本索引追踪进度，新会话读取本文件即可恢复

## 文档模板

每个模块文档采用统一结构：

1. **概述** - 一句话描述模块职责
2. **REST API** - 端点、方法、请求参数、响应格式、功能语义、错误场景
3. **IPC 接口** - 通道名、目标协议、参数、功能语义、依赖模块
4. **数据模型** - 涉及的核心数据结构
5. **模块依赖** - 依赖谁 / 被谁依赖
6. **候选公共类型** - 可能归入公共 crate 的类型

## 模块列表

| # | 模块 | 文档 | 源码位置 | 状态 |
|---|------|------|---------|------|
| # | 模块 | 文档 | 源码位置 | 状态 |
|---|------|------|---------|------|
| 1 | 公共类型与 Trait | 01-common-types.md | `process/utils/`, `common/types/`, `common/utils/`, `common/platform/` | ⬜ 待提炼（所有模块完成后） |
| 2 | 数据模型与存储 | 02-database.md | `process/services/database/` | ✅ 已完成 |
| 3 | 认证与用户管理 | 03-auth.md | `process/webserver/auth/`, `process/bridge/authBridge.ts` | ✅ 已完成 |
| 4 | 系统设置 | 04-system-settings.md | `process/bridge/systemSettingsBridge.ts`, `process/bridge/modelBridge.ts`, `common/config/` | ✅ 已完成 |
| 5 | 会话与消息管理 | 05-conversation.md | `process/bridge/conversationBridge.ts`, `process/bridge/acpConversationBridge.ts`, `process/bridge/geminiConversationBridge.ts`, `process/task/`, `common/chat/` | ✅ 已完成 |
| 6 | AI 后端集成 | 06-ai-agent.md | `process/agent/`, `process/task/*AgentManager.ts`, `process/worker/`, `process/bridge/bedrockBridge.ts`, `process/bridge/geminiBridge.ts`, `process/bridge/remoteAgentBridge.ts`, `common/api/` | ⬜ 未开始 |
| 7 | 实时通信（WebSocket） | 07-realtime.md | `process/webserver/websocket/` | ⬜ 未开始 |
| 8 | 文件与工作区 | 08-file-workspace.md | `process/bridge/fsBridge.ts`, `process/bridge/documentBridge.ts`, `process/bridge/fileWatchBridge.ts`, `process/bridge/workspaceSnapshotBridge.ts` | ⬜ 未开始 |
| 9 | 通道集成 | 09-channel.md | `process/channels/`, `process/bridge/channelBridge.ts`, `process/bridge/weixinLoginBridge.ts`, `process/webserver/routes/weixinLoginRoutes.ts` | ⬜ 未开始 |
| 10 | 团队模式 | 10-team.md | `process/team/` | ⬜ 未开始 |
| 11 | 定时任务 | 11-cron.md | `process/services/cron/`, `process/bridge/cronBridge.ts` | ⬜ 未开始 |
| 12 | MCP 协议 | 12-mcp.md | `process/services/mcpServices/`, `process/bridge/mcpBridge.ts` | ⬜ 未开始 |
| 13 | 扩展系统 | 13-extension.md | `process/extensions/`, `process/bridge/extensionsBridge.ts`, `process/bridge/hubBridge.ts` | ⬜ 未开始 |
| 14 | 应用生命周期 | 14-app-lifecycle.md | `process/bridge/updateBridge.ts`, `process/bridge/applicationBridge.ts`, `process/bridge/webuiBridge.ts`, `process/bridge/webuiQR.ts`, `process/bridge/notificationBridge.ts`, `process/webserver/config/`, `process/webserver/middleware/`, `process/webserver/types/`, `common/update/` | ⬜ 未开始 |
| 15 | Pet 系统 | 15-pet.md | `process/pet/` | ⬜ 未开始 |
| 16 | Office 文档预览 | 16-office-preview.md | `process/bridge/officeWatchBridge.ts`, `process/bridge/pptPreviewBridge.ts`, `process/bridge/previewHistoryBridge.ts`, `process/bridge/starOfficeBridge.ts` | ⬜ 未开始 |
| 17 | Shell 与语音 | 17-shell-voice.md | `process/bridge/shellBridge.ts`, `process/bridge/shellBridgeStandalone.ts`, `process/bridge/speechToTextBridge.ts` | ⬜ 未开始 |
| 99 | Rust Crate 映射 | 99-rust-crate-mapping.md | （从所有模块推导） | ⬜ 待推导（所有模块完成后） |

> 以上源码位置均相对于 `AionUi-Bak/src/`。

### 不迁移（Electron / 前端专属）

以下功能依赖 Electron 桌面端 API 或属于前端代码，Rust 后端不需要实现：

| 源码 | 说明 |
|------|------|
| `process/bridge/windowControlsBridge.ts` | 窗口最大化/最小化/关闭 |
| `process/bridge/dialogBridge.ts` | 原生文件选择对话框 |
| `renderer/` | React 前端 UI（整个目录） |
| `preload/` | Electron preload 脚本（整个目录） |
| `common/adapter/` | Electron/Browser/Standalone 通信适配层（Rust 单运行时不需要） |
| `common/platform/Electron*` | Electron 平台实现（`IPlatformServices` 接口定义可作为架构参考） |
| `common/electronSafe.ts` | Electron 环境检测 |
| `process/services/i18n/` | 主进程 i18n 服务。原项目用于托盘菜单、桌面宠物菜单等 Electron 原生 UI 的翻译。Rust 后端不做翻译，仅存储用户语言偏好并广播变更，翻译由前端自行处理 |
| `server/` | 空目录，无内容 |

**Electron / Rust 边界原则**：不迁移的功能仍保留在 Electron 薄层中。但要注意区分"触发动作"和"处理逻辑"——例如 `dialogBridge` 中"弹出文件选择对话框"是 Electron 原生能力，但"用户选完文件后的处理"属于业务逻辑，应由 Rust 后端提供 API。梳理每个模块时需按此原则划分：Electron 层只保留原生 OS 交互，所有业务逻辑迁入 Rust。

## 工作流

每个模块的梳理流程：

1. 读取本文件，找到下一个 `⬜ 未开始` 的模块（按梳理顺序）
2. 读取 `AionUi-Bak` 对应模块的源码，分析接口
3. 参考 `02-database.md` 的格式，产出 `XX-module-name.md` 文档
4. **等待用户 review 确认**（或根据反馈修改）
5. 用户确认后：
   - 更新本文件对应模块状态为 `✅ 已完成`
   - 提交 commit 并推送 push（模块文档 + 索引更新）
6. 询问用户是否继续下一个模块

> 每个模块通常在独立会话中完成。新会话从第 1 步开始。

## 梳理顺序

按依赖拓扑排序，基础模块优先：

```
数据库 (2) → 认证 (3) → 系统设置 (4)
    → 会话 (5) → AI 后端 (6) → 实时通信 (7)
    → 文件与工作区 (8) → 通道 (9) → 团队 (10)
    → 定时任务 (11) → MCP (12) → 扩展 (13) → 应用生命周期 (14)
    → Pet 系统 (15) → Office 文档预览 (16) → Shell 与语音 (17)
    → 公共类型 (1) → Crate 映射 (99)
```

## Rust Workspace 结构（初步）

```
aionui-backend/
├── Cargo.toml                    # workspace 根配置
├── crates/
│   ├── aionui-common/            # 公共类型、错误定义、工具函数
│   ├── aionui-db/                # 数据库层（SQLite、迁移、Repository trait）
│   ├── aionui-api-types/         # HTTP/WS 请求响应 DTO
│   ├── aionui-auth/              # 认证与用户管理
│   ├── aionui-conversation/      # 会话与消息管理
│   ├── aionui-ai-agent/          # AI 后端集成
│   ├── aionui-realtime/          # WebSocket 实时通信
│   ├── aionui-file/              # 文件与工作区管理
│   ├── aionui-channel/           # 通道集成（Telegram、Slack 等）
│   ├── aionui-team/              # 团队模式
│   ├── aionui-cron/              # 定时任务
│   ├── aionui-mcp/               # MCP 协议
│   ├── aionui-extension/         # 扩展系统
│   ├── aionui-system/            # 系统设置 + 应用生命周期
│   ├── aionui-pet/               # Pet 系统
│   ├── aionui-office/            # Office 文档预览
│   ├── aionui-shell/             # Shell 执行与语音
│   └── aionui-app/               # 顶层组装：路由、启动入口
```

> 此结构为初步规划，最终映射将在 `99-rust-crate-mapping.md` 中确定。

## Crate 间通信原则

- Crate 之间通过 trait 通信，不直接依赖具体实现
- 依赖方向严格向下，禁止循环依赖
- `aionui-app` 是唯一知道所有 crate 的地方，负责依赖注入和组装
- `aionui-common` 是最底层，零业务逻辑
