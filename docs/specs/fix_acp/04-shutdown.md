# ACP 协议集成：进程生命周期与 Graceful Shutdown

> **范围**: `CliAgentProcess` + `AcpAgentManager` + `idle_scanner`
> **约束**: 进程管理在 `cli_process.rs`。
> 协议拆卸在 `acp_protocol.rs`。业务决策（何时 kill、何时 stop）在 `acp_agent.rs`。

---

## 1. 生命周期状态

ACP agent 经历以下状态：

```
  Spawned ──► Initializing ──► Ready ──► Prompting ──► Ready ──► ...
     │              │             │           │
     │         (init 失败)       │      (进程崩溃)
     │              │             │           │
     ▼              ▼             ▼           ▼
  SpawnFailed   StartupCrash   Killed    Disconnected
```

**各转换的所有者：**

| 转换 | 所有者 | 机制 |
|------|--------|------|
| → Spawned | `CliAgentProcess::spawn()` | tokio `Command::spawn()` |
| → Initializing | `AcpProtocol::connect()` | SDK `initialize` 握手 |
| → Ready | `AcpProtocol::connect()` 返回 `Ok` | 收到 initialize response |
| → Prompting | `AcpAgentManager::send_message()` | 调用 `protocol.prompt()` |
| → SpawnFailed | `CliAgentProcess::spawn()` 返回 `Err` | OS 错误 |
| → StartupCrash | `AcpProtocol::connect()` 返回 `Err(StartupCrash)` | 进程在 init 完成前退出 |
| → Killed | `AcpAgentManager::kill()` | 显式终止请求 |
| → Disconnected | SDK 连接关闭 + exit watcher 触发 | 进程崩溃或被外部 kill |

## 2. Graceful Shutdown 序列

### `AcpAgentManager::kill()`

这是有序关闭，由以下场景触发：
- 用户在 UI 点击 "停止"
- Idle scanner 超时回收
- 对话删除清理

```
AcpAgentManager::kill(reason)
    │
    ├─► 1. 标记 self.closing = true
    │      （阻止 disconnect handler 做恢复工作）
    │
    ├─► 2. protocol.cancel(session_id)     [尽力而为，忽略错误]
    │      （通过 ACP cancel notification 告诉 agent 停止当前工作）
    │
    ├─► 3. process.kill(grace_period)
    │      │
    │      ├─► 3a. close_stdin()
    │      │       （agent 看到 EOF，应自行退出）
    │      │
    │      ├─► 3b. 等待 grace_period (500ms)
    │      │       （事件驱动，通过 watch channel，非轮询）
    │      │
    │      └─► 3c. 如果仍在运行：SIGKILL
    │              （通过 `kill -9 <pid>` 强制终止）
    │
    └─► 4. Status → Finished
```

**为什么是这个顺序：**
- 步骤 2 给 agent 一个机会干净地结束 session（flush 缓冲区、保存状态）。
- 步骤 3a 是标准 Unix "我不再发送输入了" 信号。
- 步骤 3b 给行为良好的 agent 时间退出。
- 步骤 3c 是应对不听话 agent 的安全网。

### `AcpAgentManager::stop()`

Stop 不是 kill。它取消当前 prompt 但保持进程存活，以便后续 prompt：

```
AcpAgentManager::stop()
    │
    ├─► 1. protocol.cancel(session_id)
    │
    ├─► 2. 清除 pending confirmations
    │
    └─► 3. Status → Finished（本轮结束，不是进程结束）
```

## 3. 进程崩溃检测

### 现有方案（保留）

`CliAgentProcess` 已经通过 `watch::channel` 实现了可靠的崩溃检测：

```rust
// cli_process.rs — exit monitor task
tokio::spawn(async move {
    match child.wait().await {
        Ok(status) => { let _ = exit_tx.send(Some(status)); }
        Err(e) => { let _ = exit_tx.send(None); }
    }
});
```

任何代码都可以检查 `process.is_running()` 或 `process.wait_for_exit()`。

### 新增：SDK 连接关闭信号

进程崩溃时，两件事几乎同时发生：

1. `CliAgentProcess` exit watcher 触发（OS 层面）
2. SDK `ByteStreams` reader 遇到 EOF → SDK 连接关闭 →
   所有 pending `SentRequest` 收到 `Err("response never received")`

我们用 **SDK 连接关闭**作为协议层清理的主信号，
用**进程 exit watcher** 作为进程层清理的主信号。

### 串联

在 `acp_protocol.rs` 中：

当 SDK 后台任务退出（因为 transport EOF）：

1. `AcpProtocol::is_connected()` 返回 `false`
2. 任何 in-flight 的 `prompt().await` 收到 `Err(AcpError::Disconnected { ... })`
3. `AcpAgentManager` 收到错误并更新状态

在 `acp_agent.rs` 中，检测连接关闭：

```rust
// 简化 — 实际实现使用 SDK handler 生命周期
if !self.protocol.is_connected() {
    let exit_info = self.process.exit_status();
    let stderr = self.process.take_stderr();
    // 记录完整诊断信息
    error!(exit_code = ?exit_info, stderr = %stderr, "ACP process crashed");
    // 更新状态
    state.status = Some(ConversationStatus::Finished);
}
```

## 4. Stderr 捕获

Stderr 是有价值的诊断信息，但它**不是协议数据**。

### 设计

`CliAgentProcess` 在环形缓冲区中捕获 stderr（最后 8 KB）：

```rust
struct CliAgentProcess {
    // ... 现有字段 ...
    stderr_buffer: Arc<Mutex<String>>,  // 环形缓冲区，最大 8192 字节
}
```

stderr 后台任务追加内容并截断前端：

```rust
tokio::spawn(async move {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut buf = stderr_buf.lock().await;
        buf.push_str(&line);
        buf.push('\n');
        if buf.len() > 8192 {
            let cut = buf.len() - 8192;
            buf.drain(..cut);
        }
        // 同时立即记录日志
        warn!(pid, stderr = line.trim(), "CLI process stderr");
    }
});
```

`take_stderr()` 方法返回缓冲区内容（消耗性的）：

```rust
pub async fn take_stderr(&self) -> String {
    let mut buf = self.stderr_buffer.lock().await;
    std::mem::take(&mut *buf)
}
```

stderr 用于 `AcpError::StartupCrash` 和 `AcpError::Disconnected` 的日志记录
— 永远不暴露在 HTTP 响应中。

## 5. Idle Scanner 集成

Idle scanner（`idle_scanner.rs`）当前在 5 分钟不活动后调用 `kill()` 终止已完成的
ACP agent。这一行为不变。

协议集成后，`kill()` 多了 cancel 步骤（§2），但 idle scanner 不需要知道这些
— 它照常调用 `kill(Some(AgentKillReason::IdleTimeout))` 即可。

**不做 suspend/resume。** TS 参考实现有 `IdleReclaimer`，在空闲时挂起 session
（拆掉进程，保留 session ID 供之后恢复）。我们不实现这个。原因：

1. 需要 SDK 的 `unstable_session_resume` 支持
2. 需要 agent 后端实际支持 resume（并非全部支持）
3. 当前 kill-and-recreate 模型对 Rust 后端足够好用
4. 将来可以作为优化加入，不影响现有接口

## 6. 超时策略

| 操作 | 超时 | 位置 | 机制 |
|------|------|------|------|
| 进程 spawn | OS 级别（几乎瞬间） | `CliAgentProcess::spawn()` | `Command::spawn()` 错误 |
| ACP initialize | 30s | `AcpProtocol::connect()` | `tokio::time::timeout` |
| ACP prompt | 无（agent 控制时长） | 不适用 | Agent 发送 `session/update` 事件；调用方可 `cancel()` |
| ACP cancel | 5s | `AcpProtocol::cancel()` | `tokio::time::timeout` |
| Kill 优雅等待 | 500ms | `CliAgentProcess::kill()` | `tokio::time::timeout` on watch channel |
| Kill SIGKILL | 5s 安全超时 | `CliAgentProcess::kill()` | SIGKILL 后等待一小段，然后放弃 |
| 空闲超时 | 5 分钟 | `idle_scanner.rs` | 周期性扫描，现有逻辑 |

**Prompt 没有超时**，因为：
- 有些 prompt 合理地需要几分钟（大规模代码分析）
- Agent 发送流式 `session/update` 事件 — UI 展示进度
- 用户可以随时通过 `stop()` 取消
- 如果进程崩溃，断连检测处理它（§3）

## 7. 变更与保留总结

| 组件 | 状态 |
|------|------|
| `CliAgentProcess::spawn()` | 保留 |
| `CliAgentProcess::kill()` | 保留（三阶段：stdin close → grace → SIGKILL） |
| `CliAgentProcess::is_running()` / `wait_for_exit()` | 保留 |
| `CliAgentProcess` stdout JSON 解析 | **移除**（SDK 接管 transport） |
| `CliAgentProcess` stderr 捕获 | **增强**（环形缓冲区用于诊断） |
| `CliAgentProcess::send()` | **移除**（协议方法替代 raw stdin 写入） |
| `AcpAgentManager::kill()` | **增强**（进程 kill 前先 ACP cancel） |
| `AcpAgentManager::stop()` | **重写**（使用 `protocol.cancel()` 替代 stdin JSON） |
| `idle_scanner.rs` | 保留（照常调用 `kill()`） |
| 断连检测 | **新增**（SDK 连接关闭 + 进程 exit watcher） |
| 启动失败检测 | **新增**（initialize 握手超时） |
