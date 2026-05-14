# Phase 2 — 最小闭环 · 技术设计

## 概述

`cargo run -p aion-cli` → 输入消息 → 流式收到 agent 回复 → 可退出。

本文档定义 Phase 2 的技术实现方案，包括架构、依赖、模块职责、数据流和生命周期。

---

## 架构总览

```
┌─────────────────────────────────────────────┐
│  aion-cli (binary crate)                    │
│                                             │
│  main.rs → clap 解析 → App::run()           │
│                                             │
│  ┌─────────┐  ┌─────────┐  ┌────────────┐  │
│  │ event   │  │   app   │  │    ui      │  │
│  │(terminal│→ │(状态机)  │→ │(ratatui    │  │
│  │ +tick)  │  │         │  │ 渲染)      │  │
│  └─────────┘  └─────────┘  └────────────┘  │
│       ↑                          ↑          │
│  ┌─────────┐              ┌────────────┐    │
│  │ client  │←─WebSocket──→│  后端      │    │
│  │(reqwest │──REST API───→│(aion serve │    │
│  │+tungstenite)           │ --local)   │    │
│  └─────────┘              └────────────┘    │
└─────────────────────────────────────────────┘
```

---

## 关键决策

| 决策点 | 选择 | 理由 |
|--------|------|------|
| 后端通信 | REST + WebSocket | 复用已有后端能力，POST 发消息，WebSocket 收流 |
| 认证 | local 模式跳过 | `aion serve --local` 已支持，降低 Phase 2 复杂度 |
| Crate 结构 | 单 crate 多模块 | 简单直接，Phase 2 够用 |
| 事件循环 | `tokio::select!` 双通道 | 终端事件 + WebSocket 消息 + tick 三路复用 |

---

## 依赖选型

| 用途 | 库 | 版本策略 |
|------|-----|---------|
| TUI 框架 | `ratatui` + `crossterm` | 跟随最新稳定版 |
| CLI 解析 | `clap` (derive) | 项目已用，版本对齐 |
| HTTP 客户端 | `reqwest` | 仅需 json feature |
| WebSocket 客户端 | `tokio-tungstenite` | 异步 WS |
| 序列化 | `serde` + `serde_json` | 与后端协议一致 |
| 异步运行时 | `tokio` (multi-thread) | 项目标准 |

---

## Crate 结构

```
crates/aion-cli/
├── Cargo.toml
├── docs/                    # 设计文档（已有）
└── src/
    ├── main.rs              # clap CLI 入口 + tokio runtime
    ├── app.rs               # App 状态机
    ├── event.rs             # 事件循环 + 终端事件收集
    ├── ui.rs                # ratatui 渲染
    ├── client.rs            # HTTP + WebSocket 客户端
    └── config.rs            # 配置加载（Phase 2 极简）
```

---

## 模块详细设计

### `main.rs` — CLI 入口

- clap 定义 `chat` 子命令
  - `--agent <TYPE>` — Agent 类型（默认从配置读取）
  - `--model <MODEL>` — 模型覆盖
  - `--server-url <URL>` — 后端地址（默认 `http://127.0.0.1:3456`）
- 初始化 tokio multi-thread runtime
- 调用 `App::new(config).run()`

### `app.rs` — 状态机核心

```rust
enum AppState {
    Connecting,   // 正在连接后端
    Idle,         // 等待用户输入
    Sending,      // 消息已发送，等待回复
    Streaming,    // 正在接收流式回复
}

struct App {
    state: AppState,
    messages: Vec<ChatMessage>,
    input: String,
    conversation_id: Option<String>,
    scroll_offset: u16,
    should_quit: bool,
}

struct ChatMessage {
    role: MessageRole,  // User | Assistant | System
    content: String,
}
```

职责：
- `handle_terminal_event(event)` — 处理键盘输入，更新 input/state
- `handle_server_event(msg)` — 处理 WebSocket 消息，追加/修改消息列表
- `send_message()` — 触发 HTTP 发送，切换状态

### `event.rs` — 事件循环

```rust
enum AppEvent {
    Terminal(crossterm::event::Event),
    Server(ServerMessage),
    Tick,
}
```

实现：
- 专用 `std::thread` poll crossterm 事件（`crossterm::event::poll` 是阻塞的）
- 通过 `tokio::sync::mpsc` 发送到主循环
- WebSocket 接收在独立 tokio task 中，通过另一个 mpsc 发送
- 主循环 `tokio::select!` 三路复用：

```rust
loop {
    tokio::select! {
        Some(event) = terminal_rx.recv() => {
            app.handle_terminal_event(event);
        }
        Some(msg) = ws_rx.recv() => {
            app.handle_server_event(msg);
        }
        _ = tick.tick() => {
            // 每 16ms 重绘一次（~60fps）
        }
    }

    terminal.draw(|f| ui::render(f, &app))?;

    if app.should_quit {
        break;
    }
}
```

### `ui.rs` — 渲染

三区域布局：

```
┌─────────────────────────────────────────────┐
│ [agent · model]              [session: xxx] │  ← 状态栏 (1 行)
├─────────────────────────────────────────────┤
│                                             │
│  You: ...                                   │  ← 消息区 (剩余空间)
│  Assistant: ...▌                            │
│                                             │
├─────────────────────────────────────────────┤
│ > 输入内容...                                │  ← 输入区 (1 行)
└─────────────────────────────────────────────┘
```

Phase 2 渲染规则：
- 纯文本，不做 Markdown 解析
- 流式输出时末尾显示 `▌` 光标
- 消息超出屏幕自动滚到底部
- 状态栏显示连接状态和 agent 类型

### `client.rs` — 后端通信

```rust
struct AionClient {
    server_url: String,
    http: reqwest::Client,
    ws_tx: Option<mpsc::Sender<WsCommand>>,
}

enum ServerMessage {
    StreamText { content: String },
    StreamFinish,
    StreamError { message: String },
    Connected,
    Disconnected,
}
```

API 调用：
- `connect()` — 建立 WebSocket 连接到 `/ws`，`Sec-WebSocket-Protocol` 传空（local 模式）
- `create_conversation(agent_type, model)` — `POST /api/conversations`
- `send_message(conversation_id, content)` — `POST /api/conversations/:id/messages`

WebSocket 消息解析：
- `{"name":"message.stream","data":{"type":"text","data":{"content":"..."}}}` → `StreamText`
- `{"name":"message.stream","data":{"type":"finish",...}}` → `StreamFinish`
- `{"name":"message.stream","data":{"type":"error",...}}` → `StreamError`

### `config.rs` — 配置（Phase 2 极简）

```rust
struct CliConfig {
    server_url: String,     // 默认 http://127.0.0.1:3456
    agent_type: String,     // 默认 "claude"
    model: Option<String>,  // 可选模型覆盖
}
```

来源优先级：CLI 参数 > 环境变量 (`AION_SERVER_URL`) > 默认值

---

## 数据流

### 发送消息

```
用户按 Enter
  → app.input 非空 → 移入 messages 列表 (role=User)
  → app.state = Sending
  → client.send_message(conversation_id, text)  [HTTP POST]
  → 后端返回 { msg_id }
  → 等待 WebSocket 推流
```

### 接收流式回复

```
WebSocket 收到 message.stream (type=text)
  → ServerMessage::StreamText { content }
  → app.state = Streaming
  → 追加 content 到 messages 最后一条 (role=Assistant)

WebSocket 收到 message.stream (type=finish)
  → ServerMessage::StreamFinish
  → app.state = Idle
  → 移除流式光标
```

### 会话初始化

```
启动 → AionClient::connect() → WebSocket 建立
     → AionClient::create_conversation(agent, model)
     → 保存 conversation_id
     → app.state = Idle
```

---

## 生命周期

1. **启动** — 解析 CLI 参数 → 初始化终端 raw mode → 连接 WebSocket → 创建 conversation
2. **运行** — 事件循环处理输入和流式回复
3. **退出** — Ctrl+C → `app.should_quit = true` → 恢复终端状态 → 断开 WebSocket → 进程退出

---

## 错误处理（Phase 2 最小集）

| 场景 | 行为 |
|------|------|
| WebSocket 连接失败 | 打印错误到 stderr，退出进程 |
| HTTP 请求失败 | 在消息区显示红色 `[Error: ...]`，回到 Idle |
| WebSocket 断开 | 状态栏显示 `[disconnected]`，退出 |
| 后端返回错误 | 在消息区显示错误内容 |

---

## 与后端交互的协议细节

### 创建会话

```http
POST /api/conversations
Content-Type: application/json

{
  "type": "claude",
  "model": {"provider": "claude", "model": "opus"},
  "source": "cli",
  "extra": {}
}
```

### 发送消息

```http
POST /api/conversations/:id/messages
Content-Type: application/json

{
  "content": "用户输入的文本",
  "files": [],
  "inject_skills": [],
  "hidden": false
}
```

### WebSocket 事件格式

```json
{
  "name": "message.stream",
  "data": {
    "conversation_id": "...",
    "msg_id": "...",
    "type": "text",
    "data": { "content": "流式文本片段" },
    "hidden": false
  }
}
```

终止事件 type 为 `"finish"` 或 `"error"`。

---

## 不在 Phase 2 范围内

- Markdown 渲染、代码高亮 → Phase 3
- 多行输入（Shift+Enter）→ Phase 3
- 会话恢复/历史 → Phase 4
- 指令系统 → Phase 4
- 认证流程（login）→ Phase 4+
- Team 模式 → Phase 5
- 配置文件系统 → Phase 4+
