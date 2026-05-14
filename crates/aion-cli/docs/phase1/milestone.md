# Aion CLI — Milestone 总览

每个 Phase 的终极目标。Phase 1 产出蓝图，Phase 2 起按文档执行。

---

## Phase 1 — 蓝图

**目标：规划一切，不写代码。**

产出物：
- 本文件（milestone.md）
- 各 phase 的详细任务清单
- 技术选型调研结论

完成标志：后续所有 phase 的任务清单已就位，开发者拿到文档即可开工。

---

## Phase 2 — 最小闭环

**目标：TUI 启动 → 输入一句话 → 流式收到 agent 回复 → 退出。**

范围限定：
- 单 agent，单轮/多轮纯文本对话
- 无布局美化，无 Markdown 渲染
- 连接参数可 hardcode 或从环境变量读取
- 流式输出逐字渲染，能中断（Ctrl+C 退出）
- 证明 TUI 框架 + 后端通信链路完整跑通

完成标志：`cargo run -p aion-cli` 启动后能和后端 agent 完成一次流式对话。

---

## Phase 3 — 完整 Chat 模式

**目标：Chat 模式达到日常可用水平。**

范围：
- 完整三段布局：状态栏 / 消息区 / 输入区
- Markdown 渲染（加粗、斜体、列表）
- 代码块语法高亮 + 语言标签
- 多行输入（Shift+Enter）
- 消息区滚动（Page Up/Down）
- Esc 中断 agent 回复
- 输入历史（↑/↓ 浏览）
- 工具调用折叠渲染

完成标志：单 agent 聊天体验接近设计稿描述，可替代简单 curl 调试。

---

## Phase 4 — Home + 会话管理 + 指令系统

**目标：具备完整的会话生命周期管理和指令入口。**

范围：
- Home 视图：最近对话列表、Agent 选择、快速操作
- 会话管理：新建 / 恢复 / 删除
- `/` 指令面板：模糊搜索、分组显示
- 核心指令实现：/agent、/model、/new、/history、/compact、/think
- Tab 补全（指令 / 文件路径）

完成标志：用户启动后能浏览历史、选择 agent、通过指令切换模型，完整会话生命周期闭合。

---

## Phase 5 — Team 模式

**目标：多 agent 协作在 TUI 中可视化。**

范围：
- 左侧成员面板（状态图标：Working/Idle/Error）
- 全局视图：所有成员消息按时间交错
- 单成员视图：Shift+←/→ 切换
- 多 agent 并行流式输出
- @mention 路由指定成员
- Team 状态栏（mode/working count/session）

完成标志：`aion team chat <ID>` 进入团队模式，能观察多成员协作全过程。

---

## Phase 6 — 打磨与发布

**目标：生产级体验，可面向用户发布。**

范围：
- 响应式布局：窄终端自动隐藏侧面板
- 断线重连 + 状态栏警告
- 终端 bell 通知（可配置）
- 无 true-color 终端降级渲染
- 配置文件支持（~/.config/aion/config.toml）
- 自定义键位映射
- 错误边界处理：崩溃恢复、优雅退出
- 性能优化：大量消息时的虚拟滚动

完成标志：通过内部 dogfood，无阻塞性体验问题，可作为默认客户端使用。
