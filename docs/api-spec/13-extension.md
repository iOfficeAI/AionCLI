# 13 - 扩展系统

## 概述

扩展系统（Extension System）：管理第三方和内置扩展的完整生命周期（加载、激活、禁用、卸载），解析扩展贡献（ACP 适配器、MCP 服务器、助手、代理、技能、主题、频道插件、WebUI、设置选项卡、模型提供商），提供沙箱隔离执行、权限控制、依赖管理和扩展市场（Hub）安装。

**源码位置**：`process/extensions/`、`process/bridge/extensionsBridge.ts`、`process/bridge/hubBridge.ts`

> **设计决策**：扩展系统是 AionUi 的可扩展性核心。原实现使用单例 `ExtensionRegistry` 管理所有扩展，通过 Zod Schema 校验清单，Worker Thread 沙箱隔离执行，子进程 fork 运行生命周期钩子。Rust 重写时保留扩展架构的核心设计（清单声明式 + 贡献解析 + 沙箱隔离），但底层执行模型需要从 Node.js Worker Thread 迁移到 WASM 沙箱或独立进程隔离。

## 子模块划分

| 子模块 | 原始源码 | Rust 归属建议 |
|--------|---------|--------------|
| 类型定义与清单校验 | `types.ts` | `aionui-extension` |
| 注册表（核心） | `ExtensionRegistry.ts` | `aionui-extension` |
| 加载器（扫描+加载） | `ExtensionLoader.ts`、`constants.ts` | `aionui-extension` |
| 生命周期管理 | `lifecycle/` | `aionui-extension` |
| 沙箱隔离 | `sandbox/` | `aionui-extension` |
| UI 通信协议 | `protocol/` | `aionui-extension` |
| 贡献解析器（10 种） | `resolvers/` | `aionui-extension` |
| 依赖与兼容性验证 | `resolvers/utils/` | `aionui-extension` |
| Hub 扩展市场 | `hub/` | `aionui-extension` |
| IPC 桥接 | `extensionsBridge.ts`、`hubBridge.ts` | `aionui-extension`（HTTP 路由） |

---

## IPC 接口

### 扩展查询

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `extensions.getLoadedExtensions` | HTTP | 无 | `ExtensionSummary[]` | 获取所有已加载扩展的摘要信息（名称、版本、状态、来源） |
| `extensions.getThemes` | HTTP | 无 | `ResolvedTheme[]` | 获取所有扩展贡献的主题（含 CSS 内容和覆盖图像） |
| `extensions.getAssistants` | HTTP | 无 | `ResolvedAssistant[]` | 获取扩展贡献的助手定义 |
| `extensions.getAcpAdapters` | HTTP | 无 | `ResolvedAcpAdapter[]` | 获取扩展贡献的 ACP 适配器（AI 后端连接器） |
| `extensions.getAgents` | HTTP | 无 | `ResolvedAgent[]` | 获取扩展贡献的自主代理定义 |
| `extensions.getMcpServers` | HTTP | 无 | `ExtMcpServer[]` | 获取扩展贡献的 MCP 服务器配置 |
| `extensions.getSkills` | HTTP | 无 | `ResolvedSkill[]` | 获取扩展贡献的技能定义 |
| `extensions.getSettingsTabs` | HTTP | 无 | `ResolvedSettingsTab[]` | 获取扩展贡献的设置选项卡（含定位信息） |
| `extensions.getWebuiContributions` | HTTP | 无 | `WebuiContribution[]` | 获取扩展贡献的 WebUI 元数据（API 路由 + 静态资源目录） |
| `extensions.getAgentActivitySnapshot` | HTTP | 无 | `AgentActivitySnapshot` | 获取代理活动快照（缓存 3 秒） |
| `extensions.getExtI18nForLocale` | HTTP | `{ locale: string }` | `Record<string, Record<string, string>>` | 获取指定语言的扩展国际化数据 |
| `extensions.getPermissions` | HTTP | `{ name: string }` | `PermissionSummary` | 获取指定扩展的权限摘要（含各权限项的风险等级分析） |
| `extensions.getRiskLevel` | HTTP | `{ name: string }` | `RiskLevel` | 获取指定扩展的总体风险等级 |

### 扩展管理

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `extensions.enableExtension` | HTTP | `{ name: string }` | `void` | 启用扩展，触发 `stateChanged` 事件通知前端 |
| `extensions.disableExtension` | HTTP | `{ name: string, reason?: string }` | `void` | 禁用扩展（可附原因），触发 `stateChanged` 事件 |

### 扩展事件

| 事件名 | 目标协议 | 载荷 | 功能语义 |
|--------|---------|------|---------|
| `extensions.stateChanged` | WebSocket | `{ name: string, enabled: boolean }` | 扩展启用/禁用状态变化时推送给前端 |

### Hub 扩展市场

| 通道名 | 目标协议 | 参数 | 返回值 | 功能语义 |
|--------|---------|------|--------|---------|
| `hub.getExtensionList` | HTTP | 无 | `HubExtensionWithStatus[]` | 加载本地+远程索引，返回带运行时状态的扩展列表 |
| `hub.install` | HTTP | `{ name: string }` | `{ success: boolean, msg?: string }` | 从 Hub 安装扩展：下载 → 解压 → 验证贡献 → 热重载注册表 |
| `hub.retryInstall` | HTTP | `{ name: string }` | `{ success: boolean, msg?: string }` | 重试安装失败的扩展 |
| `hub.checkUpdates` | HTTP | 无 | `HubUpdateInfo[]` | 检查所有已安装扩展是否有更新（当前返回空数组，TODO） |
| `hub.update` | HTTP | `{ name: string }` | `{ success: boolean, msg?: string }` | 更新指定扩展到最新版本 |
| `hub.uninstall` | HTTP | `{ name: string }` | `{ success: boolean, msg?: string }` | 卸载扩展（当前未实现） |

---

## 核心流程

### 扩展初始化流程

```
应用启动 → ExtensionRegistry.initialize()
    ↓
ExtensionLoader.loadAll()
    ├─ 扫描扩展来源目录（按优先级）：
    │   ├─ 环境变量 AIONUI_EXTENSIONS_PATH（最高优先级）
    │   ├─ 用户数据目录 ~/.aionui/extensions
    │   └─ AppData 目录（平台相关）
    ├─ 逐目录查找 aion-extension.json 清单文件
    ├─ 用 ExtensionManifestSchema (Zod) 校验清单
    └─ 返回 LoadedExtension[] （含 source 标记）
    ↓
filterByEngineCompatibility(extensions)
    ├─ 校验 engine.aionui 版本范围
    ├─ 校验 apiVersion 兼容性
    └─ 过滤不兼容的扩展（记录警告日志）
    ↓
validateDependencies(extensions)
    ├─ 检测缺失依赖
    ├─ 检测版本不匹配（支持 ^、~ 和精确版本）
    ├─ 检测循环依赖
    └─ 返回 { valid, issues[], loadOrder[] }
    ↓
sortByDependencyOrder(extensions, loadOrder)
    → 按拓扑排序确定激活顺序
    ↓
loadPersistedStates()
    → 从 ~/.aionui/extension-states.json 加载持久化状态（启用/禁用/版本）
    ↓
遍历扩展，按拓扑序逐个激活：
    activateExtension(ext, isFirstTime)
        ├─ needsInstallHook() → 首次安装或版本变更时执行 onInstall（超时 120s）
        ├─ 执行 onActivate（超时 30s）
        ├─ 生命周期钩子在子进程中运行（child_process.fork 隔离）
        └─ 激活成功 → 发出 EXTENSION_ACTIVATED 事件
    ↓
解析所有贡献（resolveContributions）：
    ├─ resolveAcpAdapters()     → ACP 适配器
    ├─ resolveMcpServers()      → MCP 服务器
    ├─ resolveAssistants()      → 助手（支持异步加载上下文文件）
    ├─ resolveAgents()          → 自主代理（支持异步加载上下文文件）
    ├─ resolveSkills()          → 技能
    ├─ resolveThemes()          → 主题（读取 CSS 文件内容）
    ├─ resolveChannelPlugins()  → 频道插件（动态加载，计划迁移到沙箱）
    ├─ resolveWebuiContributions() → WebUI 贡献（验证 API 路由命名空间）
    ├─ resolveSettingsTabs()    → 设置选项卡（支持相对定位）
    ├─ resolveModelProviders()  → 模型提供商
    └─ resolveExtensionI18n()   → 国际化数据
    ↓
savePersistedStates(states)
    → 持久化扩展状态到磁盘（debounced 500ms）
```

### Hub 安装流程

```
用户在 Hub 界面点击"安装"
    ↓
hub.install({ name })
    ↓
hubInstaller.install(name)
    ├─ hubIndexManager.getExtension(name) → 获取扩展元数据
    ├─ 从远程/本地源下载扩展包
    ├─ 解压到 getInstallTargetDir()
    ├─ verifyInstallation() → 逐个验证贡献：
    │   ├─ ACP 适配器：通过 AcpDetector 检查 CLI 是否已安装
    │   ├─ MCP 服务器：校验配置格式
    │   ├─ 助手/代理/技能：校验必填字段
    │   ├─ WebUI：验证目录和路由命名空间
    │   └─ 其他贡献：基础格式校验
    └─ ExtensionRegistry.hotReload() → 热重载注册表
    ↓
返回 { success: true } 或 { success: false, msg: errorMessage }
```

### 热重载流程

```
触发条件之一：
    ├─ Hub 安装/更新完成
    ├─ ExtensionWatcher 检测到文件变化（debounce 1000ms）
    └─ 手动调用 ExtensionRegistry.hotReload()
    ↓
ExtensionRegistry.hotReload()
    ├─ 对每个已激活扩展执行 deactivateExtension()（执行 onDeactivate 钩子）
    ├─ 销毁所有沙箱（destroyAllSandboxes）
    ├─ 清空注册表
    ├─ 重新执行初始化流程（loadAll → filter → validate → sort → activate → resolve）
    └─ 发出 REGISTRY_RELOADED 事件
```

### 沙箱执行流程

```
扩展需要运行自定义代码
    ↓
createSandbox({ extensionName, entryPoint, permissions, apiHandlers })
    ↓
SandboxHost 创建 Worker Thread
    ├─ Worker 加载 sandboxWorker.ts
    ├─ 注入受限 API（基于权限声明）
    ├─ Worker 发送 'ready' 消息（超时 10000ms）
    └─ Host 注册 API 处理器（如 ExtensionStorage 的 get/set/delete）
    ↓
扩展代码在 Worker 中运行：
    ├─ 调用 API → 发送 'api-call' 消息到 Host
    ├─ Host 执行 API → 发送 'api-response' 消息到 Worker
    ├─ 日志 → 发送 'log' 消息到 Host（记录日志）
    └─ 事件 → 发送 'event' 消息（通过 ExtensionEventBus 广播）
    ↓
销毁：
    destroySandbox(extensionName)
    ├─ 发送 'shutdown' 消息到 Worker
    ├─ 等待 Worker 终止
    └─ 清理资源
```

---

## 数据模型

### 扩展清单（aion-extension.json）

```
ExtensionManifest {
  name: string                   // 扩展唯一标识（不允许 aion-/internal-/builtin-/system- 前缀）
  version: string                // 语义化版本号
  displayName?: string           // 前端展示名称
  description?: string
  author?: string
  license?: string
  homepage?: string
  icon?: string                  // 图标文件路径（相对于扩展根目录）
  engine?: {
    aionui?: string              // 兼容的 AionUi 版本范围（如 "^1.0.0"）
  }
  apiVersion?: string            // 扩展 API 版本（当前 "1.0.0"）
  dependencies?: Record<string, string>  // 依赖的其他扩展（name → version range）
  entryPoint?: string            // 运行时入口文件（可选，用于沙箱执行）
  permissions?: ExtPermissions   // 权限声明
  contributes?: ExtContributes   // 贡献声明
  lifecycle?: LifecycleHooks     // 生命周期钩子
  i18n?: {
    locales: string[]            // 支持的语言列表
    directory?: string           // i18n 文件目录（默认 "i18n"）
  }
}
```

### 贡献声明

```
ExtContributes {
  acpAdapters?: ExtAcpAdapter[]       // ACP 后端适配器
  mcpServers?: ExtMcpServer[]         // MCP 服务器配置
  assistants?: ExtAssistant[]         // 助手定义
  agents?: ExtAgent[]                 // 自主代理定义
  skills?: ExtSkill[]                 // 技能定义
  themes?: ExtTheme[]                 // 主题
  channelPlugins?: ExtChannelPlugin[] // 频道插件
  webui?: ExtWebui[]                  // WebUI 贡献（API 路由 + 静态资源）
  settingsTabs?: ExtSettingsTab[]     // 设置选项卡
  modelProviders?: ExtModelProvider[] // 模型提供商
}
```

### 权限声明

```
ExtPermissions {
  storage?: boolean              // 持久化键值存储读写
  network?: boolean | {          // 网络访问控制
    allowedDomains: string[]     //   允许的域名列表
    reasoning: string            //   为什么需要网络访问
  }
  shell?: boolean                // 系统命令执行（危险）
  filesystem?: 'extension-only'  // 仅访问扩展自身目录
              | 'workspace'      // 访问工作区目录
              | 'full'           // 完全文件系统访问（危险）
  clipboard?: boolean            // 剪贴板读写
  activeUser?: boolean           // 访问当前活跃用户信息
  events?: boolean               // 扩展事件总线通信（默认 true）
}
```

### 风险等级

```
RiskLevel = 'safe' | 'moderate' | 'dangerous'

规则：
  - safe: 仅 storage + events
  - moderate: network（受限域名）或 filesystem=extension-only/workspace
  - dangerous: shell / filesystem=full / network=true（无域名限制）
```

### 权限摘要

```
PermissionSummary {
  permissions: ExtPermissions
  riskLevel: RiskLevel
  details: Array<{
    permission: string           // 权限项名称
    level: PermissionLevel       // 'none' | 'limited' | 'full'
    description: string          // 人类可读描述
  }>
}
```

### 扩展状态

```
ExtensionState {
  name: string
  version: string
  enabled: boolean
  installedAt?: number           // 首次安装时间戳
  lastActivatedAt?: number       // 最后激活时间戳
}
```

### 已加载扩展

```
LoadedExtension {
  manifest: ExtensionManifest
  directory: string              // 扩展根目录绝对路径
  source: ExtensionSource        // 'local' | 'appdata' | 'env'
  state: ExtensionState
}
```

### 生命周期钩子

```
LifecycleHooks {
  onInstall?: LifecycleHookValue    // 首次安装或版本变更时执行
  onUninstall?: LifecycleHookValue  // 卸载时执行
  onActivate?: LifecycleHookValue   // 每次激活时执行
  onDeactivate?: LifecycleHookValue // 去激活时执行
}

LifecycleHookValue = string      // 相对于扩展根目录的脚本路径
```

超时配置：

| 钩子 | 超时 | 说明 |
|------|------|------|
| `onInstall` | 120s | 安装可能涉及下载依赖 |
| `onUninstall` | 60s | 卸载清理 |
| `onActivate` | 30s | 每次激活 |
| `onDeactivate` | 30s | 每次去激活 |

### ACP 适配器贡献

```
ExtAcpAdapter {
  id: string                     // 唯一标识
  name: string                   // 显示名称
  description?: string
  cliCommand?: string            // CLI 命令名（如 "claude"）
  defaultCliPath?: string        // 默认 CLI 路径
  acpArgs?: string[]             // ACP 启动参数
  env?: Record<string, string>   // 环境变量（支持 ${ENV_VAR} 模板）
  avatar?: string                // 头像文件路径
  authRequired?: boolean         // 是否需要认证
  supportsStreaming?: boolean    // 是否支持流式输出
  connectionType?: string        // 连接类型
  endpoint?: string              // 远程端点 URL
  models?: string[]              // 支持的模型列表
  yoloMode?: boolean             // 无确认模式
  healthCheck?: object           // 健康检查配置
  apiKeyFields?: object[]        // API Key 配置字段
}
```

### WebUI 贡献

```
ExtWebui {
  id: string                     // WebUI 标识
  directory: string              // 静态资源目录（相对于扩展根目录）
  routes?: ExtWebuiRoute[]       // API 路由定义
}

ExtWebuiRoute {
  path: string                   // 路由路径（必须在 /{extensionName}/ 命名空间下）
  method: 'GET' | 'POST' | 'PUT' | 'DELETE'
  handler: string                // 处理器文件路径
}
```

> **设计决策**：WebUI 路由必须在 `/{extensionName}/` 命名空间下，避免与内置路由冲突。保留路径（如 `/api/`、`/auth/`、`/ws/` 等）禁止使用。

### 设置选项卡贡献

```
ExtSettingsTab {
  id: string
  label: string                  // 选项卡标题
  icon?: string                  // 图标
  url: string                    // 选项卡页面 URL（aion-asset:// 本地或 https:// 远程）
  position?: {                   // 相对于内置选项卡的定位
    relativeTo: string           //   参考选项卡 ID
    placement: 'before' | 'after'
  }
}
```

### Hub 扩展信息

```
HubExtensionWithStatus {
  name: string
  version: string
  displayName?: string
  description?: string
  author?: string
  icon?: string
  tags?: string[]
  bundled?: boolean              // 是否内置（无需下载）
  status: HubExtensionStatus     // 运行时状态
}

HubExtensionStatus = 'not_installed' | 'installed' | 'update_available'
                   | 'installing' | 'install_failed'
```

### 扩展事件

```
ExtensionSystemEvent =
  | 'EXTENSION_ACTIVATED'
  | 'EXTENSION_DEACTIVATED'
  | 'EXTENSION_INSTALLED'
  | 'EXTENSION_UNINSTALLED'
  | 'REGISTRY_RELOADED'
  | 'STATES_PERSISTED'

ExtensionLifecyclePayload {
  extensionName: string
  event: ExtensionSystemEvent
  timestamp: number
  data?: unknown
}
```

### UI 通信消息类型

```
ExtUIMessageTypes {
  // 配置
  SAVE_CONFIG: 'save-config'
  LOAD_CONFIG: 'load-config'
  CONFIG_LOADED: 'config-loaded'
  CONFIG_SAVED: 'config-saved'
  // 主题
  THEME_INFO: 'theme-info'
  THEME_CHANGED: 'theme-changed'
  // 生命周期
  WILL_DEACTIVATE: 'will-deactivate'
  DID_CLEANUP: 'did-cleanup'
  // 数据交换
  API_CALL: 'api-call'
  API_RESPONSE: 'api-response'
  // UI 状态
  UI_READY: 'ui-ready'
  RESIZE: 'resize'
}

ExtUIMessage<T> {
  type: string
  payload?: T
  requestId?: string             // 用于请求-响应配对
}
```

---

## 关键常量

| 常量 | 值 | 说明 |
|------|-----|------|
| `EXTENSION_MANIFEST_FILE` | `'aion-extension.json'` | 清单文件名 |
| `EXTENSIONS_DIR_NAME` | `'extensions'` | 扩展目录名 |
| `EXTENSION_API_VERSION` | `'1.0.0'` | 扩展 API 版本 |
| `HUB_SUPPORTED_SCHEMA_VERSION` | `1` | Hub 索引模式版本 |
| `ACTIVITY_SNAPSHOT_TTL_MS` | `3000` | 代理活动快照缓存 TTL |
| `DEBOUNCE_MS` | `1000` | 热重载防抖延迟 |
| `RESERVED_NAME_PREFIXES` | `['aion-', 'internal-', 'builtin-', 'system-']` | 保留扩展名称前缀 |
| `PRESET_AGENT_TYPES` | `['gemini', 'claude', 'codex', 'codebuddy', 'opencode']` | 预设代理类型 |

---

## 扩展扫描优先级

| 优先级 | 来源 | 路径 | 说明 |
|--------|------|------|------|
| 1（最高） | 环境变量 | `$AIONUI_EXTENSIONS_PATH` | 开发/测试用，多路径用 `:` 分隔 |
| 2 | 用户数据目录 | `~/.aionui/extensions/` | 用户安装的扩展 |
| 3 | AppData 目录 | 平台相关 | 应用内置/共享扩展 |

> E2E 测试模式（`AIONUI_E2E_TEST=1`）下仅扫描环境变量目录，保持测试隔离。

---

## 持久化存储

| 数据 | 存储位置 | 格式 | 说明 |
|------|---------|------|------|
| 扩展状态（启用/禁用/版本） | `~/.aionui/extension-states.json` | JSON | debounced 500ms 写入 |
| 扩展键值存储 | `~/.aionui/extension-storage/{extensionName}.json` | JSON | 每个扩展独立文件 |
| Hub 索引 | `{extensionsDir}/index.json` | JSON | 本地+远程合并 |

> **设计决策**：原实现使用 JSON 文件存储扩展状态。Rust 重写时建议迁移到 SQLite（`extension_states` 和 `extension_storage` 表），统一数据管理。Hub 索引可保持 JSON 文件或改为数据库缓存。

---

## 依赖管理

### 版本匹配

支持三种版本匹配方式（与 semver 兼容）：
- **精确匹配**：`"1.2.3"` → 必须完全相同
- **兼容匹配 `^`**：`"^1.2.3"` → `>=1.2.3, <2.0.0`
- **近似匹配 `~`**：`"~1.2.3"` → `>=1.2.3, <1.3.0`

### 依赖验证

```
validateDependencies(extensions)
    ├─ 遍历每个扩展的 dependencies 声明
    ├─ 检测缺失依赖 → issue: { type: 'missing', ext, dep }
    ├─ 检测版本不匹配 → issue: { type: 'version_mismatch', ext, dep, required, actual }
    ├─ 检测循环依赖 → issue: { type: 'circular', cycle: string[] }
    └─ 返回 { valid: boolean, issues: Issue[], loadOrder: string[] }
```

### 拓扑排序

依赖图无环时，返回拓扑排序的加载顺序；有环时标记为 issue 但仍尝试加载（不会阻塞启动）。

---

## 环境变量模板

扩展清单中的字符串字段支持 `${ENV_VAR}` 模板语法，在解析时替换为对应环境变量的值。

- **宽松模式**（默认）：未定义的环境变量替换为空字符串
- **严格模式**（`AIONUI_STRICT_ENV=1`）：未定义的环境变量抛出 `UndefinedEnvVariableError`

常见用途：ACP 适配器的 `env` 字段中引用用户的 API Key 等。

---

## 文件引用语法

扩展清单中支持 `@file:relative/path` 前缀语法，解析时将替换为文件内容。

```
// 清单中：
{ "systemPrompt": "@file:prompts/system.md" }

// 解析后：
{ "systemPrompt": "You are a helpful assistant..." }
```

用于助手和代理定义中的长文本字段（system prompt、context 等）。

---

## 与其他模块的集成

### 依赖

| 模块 | 依赖方式 |
|------|---------|
| `02-database` | 扩展状态持久化（Rust 重写后迁入 DB） |
| `04-system-settings` | 读取应用版本号用于引擎兼容性校验 |

### 被依赖

| 模块 | 依赖方式 |
|------|---------|
| `04-system-settings` | 扩展贡献的设置选项卡和模型提供商 |
| `05-conversation` | 扩展贡献的助手和代理定义 |
| `06-ai-agent` | 扩展贡献的 ACP 适配器和技能 |
| `07-realtime` | 扩展状态变更事件通过 WebSocket 推送 |
| `09-channel` | 扩展贡献的频道插件 |
| `12-mcp` | 扩展贡献的 MCP 服务器配置 |

---

## 外部依赖

| 库 | 用途 | Rust 替代建议 |
|----|------|--------------|
| `zod` | 清单校验（ExtensionManifestSchema） | `serde` + `jsonschema` 或自定义验证 |
| `chokidar`（推测） | 文件监听（热重载） | `notify` crate |
| `worker_threads` | Worker Thread 沙箱隔离 | `wasmtime`（WASM 沙箱）或 `tokio::process`（进程隔离） |
| `child_process` | 生命周期钩子子进程执行 | `tokio::process::Command` |
| `semver` | 版本范围匹配 | `semver` crate |

---

## 设计决策

1. **声明式清单 + 贡献解析**：扩展通过 `aion-extension.json` 声明其能力（贡献类型），注册表统一解析。这避免了命令式注册的复杂性，且可在不执行扩展代码的情况下了解其能力。Rust 重写时保留此模式。

2. **沙箱隔离模型迁移**：原实现使用 Node.js Worker Thread 作为沙箱，可执行任意 JS 代码。Rust 重写时有两个选择：
   - **WASM 沙箱**（推荐）：扩展编译为 WASM 模块，通过 `wasmtime` 运行，天然内存隔离和能力限制
   - **进程隔离**：扩展在独立子进程中运行，通过 IPC 通信
   
   WASM 方案更安全但限制了扩展的技术栈；进程隔离方案更灵活但隔离性较弱。

3. **生命周期钩子执行**：onInstall/onActivate 等钩子在子进程中运行，带超时控制。Rust 重写时用 `tokio::process::Command` 实现，保持进程级隔离和超时机制。

4. **权限声明模型**：受 Figma 插件启发，扩展需要预先声明权限。前端可据此展示风险等级，用户可知情同意。Rust 重写时在注册表层强制执行权限检查（而非仅在 UI 层展示）。

5. **热重载策略**：原实现的热重载是全量重建（deactivate all → clear → reload all）。Rust 重写时可优化为增量更新：仅重载变更的扩展，减少中断。

6. **WebUI 路由命名空间隔离**：扩展贡献的 HTTP 路由必须在 `/{extensionName}/` 前缀下，防止与内置路由或其他扩展冲突。Rust 重写时在路由注册阶段强制校验。

7. **频道插件安全问题**：原实现的 ChannelPluginResolver 使用 `eval + require` 动态加载插件代码，注释中标注计划迁移到 SandboxHost。Rust 重写时必须强制通过沙箱执行，不允许直接加载执行任意代码。

8. **Hub 索引合并策略**：本地索引和远程索引合并时，远程版本信息更新但安装状态以本地为准。`bundled` 标志表示该扩展已包含在应用中，无需下载。

---

## 候选公共类型

| 类型 | 说明 | 建议归属 |
|------|------|---------|
| `ExtensionManifest` | 扩展清单完整定义 | `aionui-extension` |
| `ExtContributes` | 贡献声明 | `aionui-extension` |
| `ExtPermissions` | 权限声明 | `aionui-extension` |
| `RiskLevel` | 风险等级枚举 | `aionui-extension` |
| `PermissionSummary` | 权限分析摘要 | `aionui-api-types` |
| `ExtensionState` | 扩展运行时状态 | `aionui-extension` |
| `LoadedExtension` | 已加载扩展信息 | `aionui-extension` |
| `ExtensionSource` | 扩展来源枚举 | `aionui-extension` |
| `ExtensionSystemEvent` | 系统事件枚举 | `aionui-extension`（导出供事件系统使用） |
| `HubExtensionWithStatus` | Hub 扩展列表项 | `aionui-api-types` |
| `ExtAcpAdapter` | ACP 适配器贡献 | `aionui-extension`（解析后提供给 `aionui-ai-agent`） |
| `ExtMcpServer` | MCP 服务器贡献 | `aionui-extension`（解析后提供给 `aionui-mcp`） |
| `WebuiContribution` | WebUI 贡献 | `aionui-extension` |
| `ResolvedSettingsTab` | 解析后的设置选项卡 | `aionui-extension` |
| `ResolvedModelProvider` | 解析后的模型提供商 | `aionui-extension`（导出供 `aionui-system` 使用） |

---

## Skill Library

> **Pilot scope (2026-04-22):** The Skill Library endpoints E1–E5 described
> below are the first module migrated under the
> [`docs/backend-migration/plans/2026-04-22-skill-library-pilot-plan.md`](../../../AionUi/docs/backend-migration/plans/2026-04-22-skill-library-pilot-plan.md)
> pilot. Renderer callers already target `/api/skills/*` via `ipcBridge.fs.*`
> (see `src/common/adapter/ipcBridge.ts` lines 301–329 in AionUi). Four of the
> five endpoints (E1, E3, E4, E5) already have a Rust implementation in
> `crates/aionui-extension/src/skill_routes.rs`; this section pins the
> pilot-required contract and calls out the deltas that Task 2 must close.

### TypeScript baseline (source of truth)

The legacy behavior is defined by the renderer-side declarations in
`AionUi/src/common/adapter/ipcBridge.ts` and the resolver / disk layout in the
Electron main process. For reference:

| ID | Renderer API                         | HTTP                             | TS resolver / disk source                                                                                                                          |
| -- | ------------------------------------ | -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| E1 | `ipcBridge.fs.listAvailableSkills`   | `GET /api/skills`                | Merge of: user skills (`~/.aionui/skills/`) + built-in skills (`src/process/resources/skills/` in dev / app-resource dir in prod) + `ExtensionRegistry.getSkills()`. Extension contributions resolved by `src/process/extensions/resolvers/SkillResolver.ts`. |
| E2 | `ipcBridge.fs.listBuiltinAutoSkills` | `GET /api/skills/builtin-auto`   | Scans `<builtin_skills_dir>/_builtin/` only (auto-injected subdirectory — see `src/process/utils/initStorage.ts:352` `getBuiltinAutoSkillsDir`). Currently contains `cron/`, `aionui-skills/`, `office-cli/`, `skill-creator/`. |
| E3 | `ipcBridge.fs.readBuiltinRule`       | `POST /api/skills/builtin-rule`  | Reads a filename under `<builtin_rules_dir>`. Returns empty string if the file is missing (graceful degradation). Traversal-safe.                  |
| E4 | `ipcBridge.fs.readBuiltinSkill`      | `POST /api/skills/builtin-skill` | Reads a filename under `<builtin_skills_dir>`. Same graceful-degradation + traversal-safe semantics as E3.                                         |
| E5 | `ipcBridge.fs.readSkillInfo`         | `POST /api/skills/info`          | Reads `<skillPath>/SKILL.md` frontmatter. `skillPath` is user-supplied. Falls back to directory name when frontmatter `name` is empty.             |

The HTTP handlers are no longer present in `src/process/bridge/` — the TS
migration commit [`5c4b010f5`](https://github.com/) moved them out; the
renderer now speaks HTTP directly to the backend, so baseline behavior must be
read from the `ipcBridge` type signatures and the `src/process/extensions/**`
resolvers. The `ExtensionRegistry._skills` cache currently only stores
`{ name, description, location }` per extension-contributed skill — the
`isCustom` and `source` fields were added to the renderer contract but never
written back into the resolver; Rust must derive them from origin directory.

### Endpoints

#### E1 — `GET /api/skills`

List every skill available to the app: built-in skills, user-imported custom
skills, and extension-contributed skills.

- **Method / path:** `GET /api/skills`
- **Request body:** none
- **Response body:** `ApiResponse<SkillListItem[]>`, sorted by `name` asc.
  ```ts
  type SkillListItem = {
    name: string;
    description: string;
    location: string;                          // absolute path on disk
    isCustom: boolean;                         // true ⇔ user-copied into userSkillsDir
    source: 'builtin' | 'custom' | 'extension';
  };
  ```
  Example:
  ```json
  {
    "success": true,
    "data": [
      {
        "name": "cron",
        "description": "Schedule recurring tasks on a cron expression",
        "location": "/Applications/AionUi.app/Contents/Resources/skills/_builtin/cron",
        "isCustom": false,
        "source": "builtin"
      },
      {
        "name": "my-skill",
        "description": "A user-imported skill",
        "location": "/Users/alice/.aionui/skills/my-skill",
        "isCustom": true,
        "source": "custom"
      }
    ]
  }
  ```
- **Source of truth on disk:** `SkillPaths.builtin_skills_dir` + user custom
  skills at `SkillPaths.user_skills_dir`, merged with
  `ExtensionRegistry.get_skills()` (once extension loading is wired in the
  backend). User skills override built-ins with the same name.
- **Error cases:** none expected under normal operation; a missing directory
  is treated as an empty list. I/O errors propagate as `500`.
- **Delta vs. current backend:** `SkillListItemResponse` in
  `crates/aionui-api-types/src/skill.rs` is missing the `source` field. Task
  2.2 must extend the struct with `source: SkillSource` (enum:
  `Builtin | Custom | Extension`, serialized lowercase) and populate it in
  `skill_service::list_available_skills`. Extension-contributed skills are not
  yet merged in — for the pilot, emit only `Builtin` / `Custom`; `Extension`
  is reserved for when `ExtensionRegistry` lands in Rust.

#### E2 — `GET /api/skills/builtin-auto`

List the subset of built-in skills that are auto-injected into every
assistant (the `_builtin/` subdirectory of the built-in skills dir).

- **Method / path:** `GET /api/skills/builtin-auto`
- **Request body:** none
- **Response body:** `ApiResponse<BuiltinAutoSkill[]>`
  ```ts
  type BuiltinAutoSkill = {
    name: string;
    description: string;
  };
  ```
  Example:
  ```json
  {
    "success": true,
    "data": [
      { "name": "cron", "description": "Schedule recurring tasks on a cron expression" },
      { "name": "skill-creator", "description": "Scaffold a new skill" }
    ]
  }
  ```
- **Source of truth on disk:** `<SkillPaths.builtin_skills_dir>/_builtin/` —
  scan each direct child directory, read its `SKILL.md` frontmatter for
  `name` and `description`.
- **Error cases:** if `_builtin/` does not exist, return an empty array (do
  not error). Unreadable `SKILL.md` entries are skipped with a warn log.
- **Delta vs. current backend:** **not implemented at all.** Task 2.3 must
  add the route, the handler, and a service function
  (`skill_service::list_builtin_auto_skills`). Reuse the existing
  `scan_skill_dirs` helper but point it at the `_builtin/` subdirectory.

#### E3 — `POST /api/skills/builtin-rule`

Read the content of a built-in rule file.

- **Method / path:** `POST /api/skills/builtin-rule`
- **Request body:**
  ```ts
  type Request = { fileName: string };
  ```
  Example: `{ "fileName": "code-review.md" }`
- **Response body:** `ApiResponse<string>` — raw file content. Empty string
  if the file does not exist (by design, matches TS fallback).
- **Source of truth on disk:** `<SkillPaths.builtin_rules_dir>/<fileName>`.
- **Error cases:**
  - `fileName` contains `/`, `\\`, `..`, or is absolute → `400 BadRequest`
    via `ExtensionError::PathTraversal`.
  - File exists but is unreadable → `500`.
  - File does not exist → `200` with empty string body (graceful, matches
    the TS baseline).
- **Delta vs. current backend:** implemented at
  `skill_routes::read_builtin_rule` and `skill_service::read_builtin_rule`.
  No change required beyond adding the `ApiResponse<String>` integration
  test under the pilot.

#### E4 — `POST /api/skills/builtin-skill`

Read the content of a file under the built-in skills directory (same shape
as E3 but for skills instead of rules).

- **Method / path:** `POST /api/skills/builtin-skill`
- **Request body:** `{ fileName: string }` — may include a relative
  sub-path like `cron/SKILL.md` (the TS baseline permits reading individual
  skill files; see `validate_filename` in `skill_service.rs`).
- **Response body:** `ApiResponse<string>` — raw file content, empty string
  when missing.
- **Source of truth on disk:** `<SkillPaths.builtin_skills_dir>/<fileName>`.
- **Error cases:** same as E3 (path traversal rejected; missing file
  returns empty).
- **Delta vs. current backend:** implemented at
  `skill_routes::read_builtin_skill` and
  `skill_service::read_builtin_skill`. Verify that `validate_filename`
  correctly allows legitimate nested paths like `cron/SKILL.md` (the
  current implementation rejects any `/`, which would break the TS
  baseline use). If so, Task 2.5 must relax the guard to reject only `..`
  segments and absolute paths, while still preventing traversal — see
  Step 2.5 in the pilot plan.
- **Open question (resolved by reading current guard):** the TS baseline
  accepts plain filenames (`"some-file.md"`); it is not clear whether the
  renderer ever requests nested paths. Inspect all renderer call sites of
  `readBuiltinSkill` / `readBuiltinRule` in Step 2.5 and adjust the guard
  to match actual call-site needs. Default: keep the strict
  "no-path-separators" guard that the backend has today; note in the
  commit body if the check had to be loosened.

#### E5 — `POST /api/skills/info`

Read the frontmatter of a `SKILL.md` at an arbitrary on-disk path.

- **Method / path:** `POST /api/skills/info`
- **Request body:**
  ```ts
  type Request = { skillPath: string };
  ```
  `skillPath` may be a directory (which will read `<dir>/SKILL.md`) or a
  direct path to the `SKILL.md` file itself.
- **Response body:** `ApiResponse<ReadSkillInfoResponse>`
  ```ts
  type ReadSkillInfoResponse = { name: string; description: string };
  ```
  Example:
  ```json
  {
    "success": true,
    "data": { "name": "cron", "description": "Schedule recurring tasks" }
  }
  ```
  When frontmatter has an empty `name`, the backend falls back to the
  directory basename of `skillPath`.
- **Source of truth on disk:** caller-supplied. There is no enclosing
  directory restriction (the UI lets the user browse to any skill folder).
- **Error cases:**
  - `skillPath` does not resolve to an existing file → `ExtensionError::SkillNotFound`.
  - `SKILL.md` has no parsable frontmatter → `ExtensionError::InvalidSkillPath`.
- **Delta vs. current backend:** implemented at
  `skill_routes::read_skill_info` and `skill_service::read_skill_info`.
  Verify that the `name`-empty fallback to directory basename is exercised
  by Task 2.6's test matrix.

### Contract summary

| ID | Method | Path                         | Request              | Response                              | Impl status (as of 2026-04-22)          |
| -- | ------ | ---------------------------- | -------------------- | ------------------------------------- | --------------------------------------- |
| E1 | GET    | `/api/skills`                | —                    | `SkillListItem[]`                     | Partial — **`source` field missing**    |
| E2 | GET    | `/api/skills/builtin-auto`   | —                    | `BuiltinAutoSkill[]`                  | **Not implemented**                     |
| E3 | POST   | `/api/skills/builtin-rule`   | `{ fileName }`       | `string`                              | Implemented                             |
| E4 | POST   | `/api/skills/builtin-skill`  | `{ fileName }`       | `string`                              | Implemented                             |
| E5 | POST   | `/api/skills/info`           | `{ skillPath }`      | `{ name, description }`               | Implemented                             |

### Deltas to close in Task 2

1. **E1 — add `source` field.** Extend `SkillListItemResponse` in
   `aionui-api-types` with `source: SkillSource` and populate it in
   `list_available_skills`. Extension-contributed entries defer to a future
   milestone; the pilot only needs `Builtin` / `Custom`.
2. **E2 — new endpoint.** Register the `/api/skills/builtin-auto` route,
   implement `skill_service::list_builtin_auto_skills` scanning
   `<builtin_skills_dir>/_builtin/`, add unit + HTTP integration tests.
3. **Integration tests for E3/E4/E5.** The existing implementations have
   unit tests in `skill_service.rs` but none of the HTTP-level tests
   prescribed by the pilot plan (`crates/aionui-extension/tests/skills_http_test.rs`).
   Add them in Steps 2.4–2.6 as `tokio::test`s using `axum::Router::oneshot`.
4. **Response envelope.** The pilot plan's contract table omits the
   `ApiResponse<T>` envelope (`{ success, data, message }`). The backend
   already wraps every response in `ApiResponse`; renderer-side HTTP bridge
   auto-unwraps `data`. No change required here — this note is for the
   frontend-dev teammate to verify against `httpGet`/`httpPost` behavior.

