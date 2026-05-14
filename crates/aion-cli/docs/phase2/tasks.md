# Phase 2 — 最小闭环 · 任务清单

## 目标

`cargo run -p aion-cli` → 输入消息 → 流式收到 agent 回复 → 可退出。

---

## 任务

### 2.1 项目骨架搭建

- [ ] 确认 TUI 框架选型（ratatui + crossterm）
- [ ] 初始化 crate 结构：main.rs、app.rs、ui.rs、event.rs
- [ ] 配置 clap 命令行参数解析（子命令：chat）
- [ ] 确认终端 raw mode 进出机制

### 2.2 事件循环

- [ ] 实现基础 event loop：终端事件 + 后端消息 双通道 select
- [ ] 键盘事件分发：Enter 发送、Ctrl+C 退出
- [ ] 实现 App 状态机（Idle → Sending → Streaming → Idle）

### 2.3 后端通信

- [ ] 对接后端 chat API（WebSocket 或 SSE）
- [ ] 实现连接建立 + 认证握手
- [ ] 实现发送消息请求
- [ ] 实现流式 token 接收与缓冲

### 2.4 最小 UI 渲染

- [ ] 消息区：纯文本渲染用户输入 + agent 回复
- [ ] 输入区：单行文本输入 + 光标
- [ ] 流式输出：逐 token 追加到最后一条消息
- [ ] 基本滚动：消息超出屏幕时自动滚到底部

### 2.5 基本生命周期

- [ ] 启动时连接后端
- [ ] Ctrl+C 优雅退出（恢复终端状态）
- [ ] 连接失败时显示错误并退出
