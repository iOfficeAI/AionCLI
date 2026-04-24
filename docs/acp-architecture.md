# ACP Architecture Analysis

## 1. Overall Architecture Overview

aionui-backend is a Rust backend server built with **Axum + Tokio + SQLite**, organized as a Cargo workspace with 17 crates across four layers.

### Layer Structure

```
┌─────────────────────────────────────────────────────────────┐
│  Composition Layer                                          │
│  ┌─────────────────────────────────────────────────────────┐│
│  │  aionui-app                                             ││
│  │  (binary entry, router assembly, DI orchestration)      ││
│  └─────────────────────────────────────────────────────────┘│
├─────────────────────────────────────────────────────────────┤
│  Domain Layer (11 crates, loosely coupled)                  │
│  ┌────────────┬────────────┬────────────┬────────────┐      │
│  │conversation│  channel   │   team     │   cron     │      │
│  ├────────────┼────────────┼────────────┼────────────┤      │
│  │ ai-agent   │   file     │  office    │  system    │      │
│  ├────────────┼────────────┼────────────┼────────────┤      │
│  │    mcp     │ extension  │   shell    │            │      │
│  └────────────┴────────────┴────────────┴────────────┘      │
├─────────────────────────────────────────────────────────────┤
│  Capability Layer (cross-cutting)                           │
│  ┌───────────────────────┬─────────────────────────────┐    │
│  │  aionui-auth          │  aionui-realtime            │    │
│  │  (JWT, CSRF, bcrypt,  │  (WebSocket, event bus,     │    │
│  │   auth middleware)    │   broadcast channels)       │    │
│  └───────────────────────┴─────────────────────────────┘    │
├─────────────────────────────────────────────────────────────┤
│  Foundation Layer (zero/minimal internal deps)              │
│  ┌─────────────────┬────────────────┬──────────────────┐    │
│  │  aionui-common  │ aionui-api-    │  aionui-db       │    │
│  │  (AppError,     │  types         │  (SQLite, sqlx,  │    │
│  │   enums, crypto,│  (request/     │   repository     │    │
│  │   ID gen,       │   response     │   traits & impls)│    │
│  │   pagination)   │   DTOs)        │                  │    │
│  └─────────────────┴────────────────┴──────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

### Dependency Direction Rules

```
Composition → Domain → Capability → Foundation
              Domain → Foundation   (cross-layer allowed)
```

- Upper layers may depend on lower layers
- Same-layer interaction only through trait abstractions (e.g., `IWorkerTaskManager`)
- No circular dependencies, no upward references

### Key Architectural Patterns

| Pattern | Description |
|---------|-------------|
| Trait-based DI | Repository traits in `aionui-db`, service traits in domain crates, all wired in `aionui-app` |
| Centralized AppServices | Single construction center for all shared services |
| Event-driven realtime | `BroadcastEventBus` + WebSocket for push-based client updates |
| Domain isolation | Each domain crate owns `routes.rs` / `service.rs` / `state.rs` |
| Worker task queue | `IWorkerTaskManager` manages per-conversation agent lifecycle |

### Domain Crate Standard Anatomy

```
crates/aionui-{domain}/src/
├── lib.rs       # Module exports only
├── routes.rs    # HTTP handlers (request/response transformation)
├── service.rs   # Business logic (sole location)
├── state.rs     # RouterState (#[derive(Clone)], Arc-wrapped deps)
├── error.rs     # Domain-specific errors (optional)
└── types.rs     # Domain models (optional)
```

---

## 2. ACP in the Overall Architecture

### Positioning

ACP (Agent Communication Protocol) is the core agent orchestration subsystem of AionUI. It lives primarily in the **aionui-ai-agent** domain crate and spans several related areas:

```
┌──────────────────────────────────────────────────────────────────┐
│                        aionui-app                                │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  Router Assembly (ACP routes, remote agent routes, etc.)   │  │
│  │  AppServices → build AcpRouterState, ConnectionTestState   │  │
│  └────────────────────────────────────────────────────────────┘  │
├──────────────────────────────────────────────────────────────────┤
│                    Domain Layer                                  │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐    │
│  │               aionui-ai-agent  ◀── ACP core              │    │
│  │  ┌──────────────────────────────────────────────────┐    │    │
│  │  │  ACP Agent      │  Remote Agent   │  Agent       │    │    │
│  │  │  (acp_agent.rs) │ (remote_agent.rs│  Factory     │    │    │
│  │  │  (acp_routes.rs)│  remote_agent_  │ (factory.rs) │    │    │
│  │  │  (acp_service.rs│  routes.rs,     │              │    │    │
│  │  │                 │  service.rs)    │              │    │    │
│  │  ├──────────────────────────────────────────────────┤    │    │
│  │  │  Task Manager   │  Stream Events  │  CLI Process │    │    │
│  │  │ (task_manager.rs│(stream_event.rs)│(cli_process) │    │    │
│  │  ├──────────────────────────────────────────────────┤    │    │
│  │  │  Skill Manager  │  Middleware     │  Idle Scanner│    │    │
│  │  │(skill_manager)  │(middleware.rs)  │(idle_scanner)│    │    │
│  │  ├──────────────────────────────────────────────────┤    │    │
│  │  │  Other Agents: Gemini, Nanobot, OpenClaw, Aionrs │    │    │
│  │  └──────────────────────────────────────────────────┘    │    │
│  └──────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐  │
│  │  conversation   │  │     team        │  │     cron        │  │
│  │  (uses IWorker- │  │  (uses IWorker- │  │  (uses IWorker- │  │
│  │   TaskManager)  │  │   TaskManager)  │  │   TaskManager)  │  │
│  └─────────────────┘  └─────────────────┘  └─────────────────┘  │
├──────────────────────────────────────────────────────────────────┤
│  Foundation: aionui-common (AgentType, AcpBackend enums)         │
│              aionui-api-types (ACP request/response types)       │
│              aionui-db (RemoteAgentRepository, OAuthTokenRepo)   │
└──────────────────────────────────────────────────────────────────┘
```

### Cross-Crate Dependencies

ACP-related code touches these crates:

| Crate | ACP-related Content |
|-------|-------------------|
| `aionui-common` | `AgentType`, `AcpBackend`, `RemoteAgentProtocol`, `RemoteAgentAuthType`, `RemoteAgentStatus`, `AgentKillReason`, `Confirmation` types |
| `aionui-api-types` | ACP request/response DTOs (`acp.rs`, `remote_agent.rs`, `connection_test.rs`, `confirmation.rs`) |
| `aionui-db` | `IRemoteAgentRepository`, `IOAuthTokenRepository`, `RemoteAgentRow`, `OAuthTokenRow`, `acp_session` table |
| `aionui-ai-agent` | Core ACP implementation (agent managers, routes, services, factory, task manager) |
| `aionui-conversation` | Orchestrates agent tasks via `IWorkerTaskManager` for message send/receive |
| `aionui-team` | Multi-agent session management, uses agent factory for team members |
| `aionui-cron` | Scheduled agent invocation via `IWorkerTaskManager` |
| `aionui-app` | Wires all ACP-related states and routes |

---

## 3. ACP Subsystem Detailed Architecture

### 3.1 Agent Type System

AionUI supports multiple agent types, all unified under the `IAgentManager` trait:

```
                    IAgentManager (trait)
                          │
          ┌───────────────┼───────────────────────┐
          │               │                       │
    AcpAgentManager  RemoteAgentManager    GeminiAgentManager
    (CLI subprocess) (WebSocket)           (CLI subprocess)
          │                                       │
    NanobotAgentManager  OpenClawAgentManager  AionrsAgentManager
    (CLI subprocess)     (WebSocket)           (CLI subprocess)
```

```rust
pub enum AgentType {
    Acp,               // CLI-based agents (20+ backends)
    Remote,            // WebSocket remote agents
    Gemini,            // Google Gemini CLI
    OpenclawGateway,   // OpenClaw WebSocket protocol
    Nanobot,           // Nanobot CLI
    Aionrs,            // Aionrs CLI
}
```

### 3.2 ACP Backend Ecosystem

ACP (`AgentType::Acp`) is the primary agent type, supporting 20+ CLI backends:

```
AcpBackend
├── CLI-based (binary in PATH)
│   ├── Claude        (claude)
│   ├── Codex         (codex)
│   ├── CodeBuddy     (codebuddy)
│   ├── Qwen          (qwen)
│   ├── Kiro          (kiro)
│   ├── OpenCode      (opencode)
│   ├── Copilot       (copilot)
│   ├── Goose         (goose)
│   ├── Cursor        (cursor)
│   ├── Droid         (droid)
│   ├── Auggie        (auggie)
│   ├── Kimi          (kimi)
│   ├── Qoder         (qoder)
│   ├── Vibe          (vibe)
│   ├── Nanobot       (nanobot)
│   ├── Hermes        (hermes)
│   └── Snow          (snow)
│
├── Non-CLI (special handling)
│   ├── IFlow         (handled separately)
│   ├── Gemini        (handled by GeminiAgentManager)
│   ├── OpenclawGateway (handled by OpenClawAgentManager)
│   ├── Remote        (handled by RemoteAgentManager)
│   └── Aionrs        (handled by AionrsAgentManager)
│
└── Custom            (user-defined command)
```

### 3.3 IAgentManager Trait (Core Interface)

All agent types implement this unified interface:

```rust
pub trait IAgentManager: Send + Sync {
    fn agent_type(&self) -> AgentType;
    fn status(&self) -> Option<ConversationStatus>;
    fn workspace(&self) -> &str;
    fn conversation_id(&self) -> &str;
    fn last_activity_at(&self) -> TimestampMs;

    // Event subscription (broadcast channel)
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent>;

    // Message operations
    async fn send_message(&self, data: SendMessageData) -> Result<(), AppError>;
    async fn stop(&self) -> Result<(), AppError>;

    // Tool confirmation flow
    fn confirm(&self, msg_id: &str, call_id: &str, data: Value, always_allow: bool)
        -> Result<(), AppError>;
    fn get_confirmations(&self) -> Vec<Confirmation>;
    fn check_approval(&self, action: &str, command_type: Option<&str>) -> bool;

    // Lifecycle
    fn kill(&self, reason: Option<AgentKillReason>) -> Result<(), AppError>;
    fn as_any(&self) -> &dyn Any;  // for downcasting to concrete type
}
```

---

## 4. Agent Discovery

### 4.1 CLI Detection

Agent discovery is performed via PATH lookup using the `which` crate:

```
User Request → /api/acp/agents → acp_service::get_available_agents()
                                      │
                                      ├─ For each known agent in predefined list:
                                      │    cli_binary_name(backend) → Option<binary_name>
                                      │    which::which(binary_name) → available: bool
                                      │
                                      └─ Returns Vec<AcpAgentInfo> { id, name, backend, available }
```

**Key endpoints:**

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/acp/agents` | GET | List known agents with availability status |
| `/api/acp/agents/refresh` | POST | Re-scan agent availability |
| `/api/acp/detect-cli` | POST | Detect specific backend CLI path |
| `/api/acp/agents/test` | POST | Test custom agent command |
| `/api/acp/health-check` | POST | Check backend availability + latency |
| `/api/acp/env` | GET | Get relevant environment variables |

### 4.2 Remote Agent Discovery

Remote agents are user-configured and persisted in the database:

```
User creates via POST /api/remote-agents
    │
    ├─ name, protocol (OpenClaw/ZeroClaw/Acp), url, auth_type
    │
    ├─ If protocol == OpenClaw:
    │    auto-generate Ed25519 device keypair
    │    encrypt and store device keys
    │
    └─ Store in remote_agents table (auth_token AES-encrypted)
```

---

## 5. Agent Authentication

### 5.1 ACP CLI Agents

CLI-based ACP agents inherit authentication from the host system. The CLI binary itself manages API keys and credentials. AionUI does not directly handle authentication for local CLI agents.

### 5.2 Remote Agent Authentication

Three authentication methods:

| Auth Type | Mechanism |
|-----------|-----------|
| `Bearer` | Token-based auth, auth_token sent as bearer token |
| `Password` | Password-based auth, auth_token contains password |
| `None` | No authentication required |

All sensitive data is AES-GCM encrypted at rest:
- `auth_token` — AES-encrypted in DB
- `device_public_key` — AES-encrypted (OpenClaw)
- `device_private_key` — AES-encrypted (OpenClaw)
- `device_token` — AES-encrypted (OpenClaw)

Encryption key: 32-byte key derived from JWT secret via `AppServices`.

### 5.3 OpenClaw Handshake Protocol

```
POST /api/remote-agents/{id}/handshake
    │
    ├─ WebSocket connect to remote agent URL (15s timeout)
    ├─ Device keypair-based handshake
    ├─ On success: status = "connected", last_connected_at = now
    └─ Returns HandshakeResponse { status: "ok" }
```

### 5.4 Connection Testing

```
POST /api/remote-agents/test-connection
    │
    ├─ Validate URL format (SSRF protection)
    ├─ WebSocket connect attempt (10s timeout)
    └─ Returns success/failure

POST /api/bedrock/test-connection
    │
    ├─ Validate AWS config (region, credentials)
    ├─ Isolated AWS SDK config (no global env pollution)
    └─ Call get_foundation_model() with test model

GET /api/gemini/subscription-status
    │
    ├─ GEMINI_API_KEY from environment
    ├─ API call to generativelanguage.googleapis.com
    └─ Returns subscription_status: "active" | "inactive"
```

---

## 6. Agent Session & Conversation Orchestration

### 6.1 Task Manager (Per-Conversation Agent Lifecycle)

The `WorkerTaskManager` maintains a 1:1 mapping of conversation → agent:

```
conversations DashMap<String, AgentManagerHandle>
    │
    ├─ get_task(conv_id) → Option<Handle>
    ├─ get_or_build_task(conv_id, options) → Handle  (lazy creation)
    ├─ kill(conv_id, reason) → ()  (cleanup)
    ├─ clear() → ()  (kill all)
    ├─ active_count() → usize
    └─ collect_idle(threshold_ms) → Vec<conv_id>  (for idle scanner)
```

### 6.2 Agent Factory (Construction Pipeline)

```
BuildTaskOptions
├── agent_type: AgentType
├── workspace: String (working directory)
├── model: ProviderWithModel
├── conversation_id: String
└── extra: serde_json::Value (type-specific config)
        │
        ├── AcpBuildExtra (for AgentType::Acp)
        │   ├── backend: AcpBackend
        │   ├── cli_path: Option<String>
        │   ├── custom_workspace: bool
        │   ├── agent_name: Option<String>
        │   ├── preset_context: Option<String>
        │   ├── enabled_skills: Vec<String>
        │   ├── session_mode: Option<String>
        │   └── cron_job_id: Option<String>
        │
        ├── RemoteBuildExtra (for AgentType::Remote)
        │   └── remote_agent_id: String
        │
        └── GeminiBuildExtra (for AgentType::Gemini)
            └── (specific fields)
```

**Factory dispatch logic:**

```rust
match agent_type {
    AgentType::Acp => {
        let extra: AcpBuildExtra = serde_json::from_value(options.extra)?;
        let process = CliAgentProcess::spawn(config)?;
        AcpAgentManager::new(process, backend, workspace, conversation_id, extra)
    }
    AgentType::Remote => {
        let extra: RemoteBuildExtra = serde_json::from_value(options.extra)?;
        let agent_row = remote_agent_repo.find_by_id(&extra.remote_agent_id).await?;
        let config = RemoteAgentConfig { /* decrypt auth_token */ };
        let manager = RemoteAgentManager::new(config, ...);
        manager.connect().await?;
        manager
    }
    AgentType::Gemini => { /* similar CLI spawn pattern */ }
    // ... other types
}
```

### 6.3 ACP Agent Session Lifecycle

```
┌─────────────────────────────────────────────────────────────┐
│  AcpAgentManager                                            │
│                                                             │
│  1. new() ─── spawn CLI subprocess                         │
│       │       pre-subscribe event receiver                  │
│       │       init AcpState { status: None }                │
│       │                                                     │
│  2. start_relay() ─── background task reads CLI stdout      │
│       │                parses AgentStreamEvent               │
│       │                updates internal state                │
│       │                broadcasts to subscribers             │
│       │                                                     │
│  3. send_message() ─── acquire session_lock                 │
│       │                                                     │
│       ├── First message: ensure_session_and_send()          │
│       │   ├── session/new (with initial context)            │
│       │   │   ├── preset_context injection                  │
│       │   │   ├── enabled_skills injection                  │
│       │   │   └── session_mode override                     │
│       │   └── status → Running                              │
│       │                                                     │
│       ├── Subsequent messages:                              │
│       │   ├── SessionLoad (Codex):                          │
│       │   │   session/load → sendMessage                    │
│       │   ├── ClaudeResumeMeta (Claude/CodeBuddy):          │
│       │   │   session/new with resume meta                  │
│       │   └── ResumeSessionId (others):                     │
│       │       session/new with resumeSessionId              │
│       │                                                     │
│  4. Event stream ─── continuous                             │
│       │   Start → set session_id                            │
│       │   Text → incremental content                        │
│       │   AcpPermission → add to confirmations list         │
│       │   AcpModelInfo → store model info                   │
│       │   Finish → set session_id, status → Finished        │
│       │   Error → status → Finished                         │
│       │                                                     │
│  5. confirm() ─── confirmMessage protocol command           │
│       │           remove from confirmations list            │
│       │           if always_allow: store in approval_memory │
│       │                                                     │
│  6. stop() ─── session/cancel                              │
│                                                             │
│  7. kill() ─── terminate CLI process (grace period 500ms)  │
│               clear from task manager                       │
└─────────────────────────────────────────────────────────────┘
```

### 6.4 Session Resume Strategies

Different backends use different session resume mechanisms:

| Strategy | Backends | Mechanism |
|----------|----------|-----------|
| `SessionLoad` | Codex | `session/load` + `sendMessage` |
| `ClaudeResumeMeta` | Claude, CodeBuddy | `session/new` with `_meta.claudeCode.options.resume` |
| `ResumeSessionId` | All others | `session/new` with `resumeSessionId` |

### 6.5 Per-Conversation Session Control

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/conversations/{id}/acp/mode` | GET/PUT | Get/set YOLO mode |
| `/api/conversations/{id}/acp/model` | GET/PUT | Get/set model |
| `/api/conversations/{id}/acp/config` | GET | Get config options |
| `/api/conversations/{id}/acp/config/{configId}` | PUT | Set config option |

---

## 7. ACP Connection & Communication

### 7.1 CLI Process Communication (ACP Agents)

```
┌──────────────┐    stdin (JSON)     ┌─────────────────┐
│ AionUI       │ ──────────────────▶ │ CLI Subprocess   │
│ Backend      │                     │ (claude, qwen,   │
│              │ ◀────────────────── │  codex, etc.)    │
│              │    stdout (JSON)    │                  │
└──────────────┘                     └─────────────────┘
```

**Protocol format (stdin → subprocess):**
```json
{
  "type": "session/new",
  "data": {
    "message": "user message",
    "workspace": "/path/to/workspace",
    "systemPrompt": "optional context",
    "_meta": { /* backend-specific metadata */ }
  }
}
```

**Protocol commands:**

| Command | Direction | Description |
|---------|-----------|-------------|
| `session/new` | → CLI | Create new session with initial message |
| `session/load` | → CLI | Load existing session (Codex only) |
| `session/cancel` | → CLI | Stop current streaming response |
| `sendMessage` | → CLI | Send message to existing session |
| `confirmMessage` | → CLI | Approve/reject tool call |
| `session/setMode` | → CLI | Enable YOLO mode |
| `session/getMode` | → CLI | Query current mode |
| `session/setModel` | → CLI | Switch model |
| `session/getModelInfo` | → CLI | Get current model info |
| `session/getConfigOptions` | → CLI | Get available config options |
| `session/setConfigOption` | → CLI | Set configuration option |
| `session/getSlashCommands` | → CLI | Get available slash commands |

### 7.2 WebSocket Communication (Remote Agents)

```
┌──────────────┐   WebSocket (JSON)  ┌─────────────────┐
│ AionUI       │ ◀════════════════▶  │ Remote Agent     │
│ Backend      │                     │ Server           │
│              │                     │ (WebSocket)      │
└──────────────┘                     └─────────────────┘
```

Remote agents use WebSocket for bidirectional communication, reusing the same `AgentStreamEvent` types as CLI agents.

### 7.3 Stream Event Types (24 Variants)

```rust
pub enum AgentStreamEvent {
    // Lifecycle
    Start { session_id: Option<String> },
    Finish { session_id: Option<String> },
    Error { message: String, code: Option<String> },

    // Content
    Text { content: String },
    Thinking { content, subject, duration, status },
    Plan { entries: Vec<PlanEntry> },

    // Tool interaction
    ToolCall { call_id, name, args, status },
    ToolGroup { calls: Vec<ToolGroupEntry> },
    AcpPermission { /* Confirmation data */ },
    AcpToolCall { /* ACP-specific tool progress */ },
    CodexPermission { /* Codex variant */ },
    CodexToolCall { /* Codex variant */ },

    // Session info
    AgentStatus { backend, status, agent_name, session_id },
    AcpModelInfo { model_id, model_name, provider },
    AcpContextUsage { /* token/context metrics */ },

    // Features
    Tips { level, message },
    AvailableCommands { commands: Vec<SlashCommandItem> },
    SkillSuggest { skill_name },
    CronTrigger { cron_job_id },
    System { content: String },
    RequestTrace { /* debug data */ },
}
```

---

## 8. Tool Confirmation & Approval Flow

### 8.1 Confirmation Lifecycle

```
CLI emits AcpPermission event
    │
    ▼
Event relay parses → Confirmation { id, call_id, action, description, options }
    │
    ▼
Added to AcpState.confirmations list
    │
    ▼
Broadcast to WebSocket subscribers → frontend shows dialog
    │
    ▼
User approves/rejects
    │
    ├── approve (always_allow=false): confirmMessage → remove from list
    ├── approve (always_allow=true):  confirmMessage → remove from list
    │                                  + store in approval_memory
    └── reject: confirmMessage with reject value → remove from list
```

### 8.2 Approval Memory (Session-Level)

```rust
// Key format
fn approval_key(action, command_type) -> String {
    match (action, command_type) {
        (Some(a), Some(ct)) => format!("{a}:{ct}"),   // "edit_file:bash"
        (Some(a), None) => a.to_owned(),              // "read_file"
        _ => String::new(),
    }
}
```

- Session-scoped (cleared on kill, not persisted across restarts)
- Used for auto-approval of repeated identical confirmations
- Applied via `check_approval(action, command_type)` before showing UI dialog

---

## 9. Idle Cleanup & Lifecycle Management

### 9.1 Idle Scanner

```
start_idle_scanner(task_manager, interval_secs, idle_threshold_ms)
    │
    └── Periodic loop (default interval):
        ├── collect_idle(threshold_ms)
        │   └── For each active task:
        │       if agent_type == Acp
        │          && status == Finished
        │          && (now - last_activity) > threshold_ms
        │       → include in idle list
        │
        └── For each idle conversation_id:
            kill(conv_id, Some(IdleTimeout))
```

### 9.2 Activity Tracking

- `last_activity: AtomicI64` — lock-free, updated on every event from CLI stdout
- `Ordering::Relaxed` — sufficient for idle detection (no strict ordering needed)

---

## 10. Database Schema (ACP-Related)

### remote_agents

```sql
CREATE TABLE remote_agents (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    protocol          TEXT NOT NULL,          -- openClaw, zeroClaw, acp
    url               TEXT NOT NULL,          -- WebSocket endpoint
    auth_type         TEXT NOT NULL,          -- bearer, password, none
    auth_token        TEXT,                   -- AES-encrypted
    allow_insecure    INTEGER NOT NULL DEFAULT 0,
    avatar            TEXT,
    description       TEXT,
    device_id         TEXT,                   -- OpenClaw device ID
    device_public_key TEXT,                   -- AES-encrypted Ed25519
    device_private_key TEXT,                  -- AES-encrypted Ed25519
    device_token      TEXT,                   -- AES-encrypted
    status            TEXT NOT NULL DEFAULT 'unknown',
    last_connected_at INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
```

### acp_session

```sql
CREATE TABLE acp_session (
    conversation_id TEXT PRIMARY KEY,
    agent_backend   TEXT NOT NULL,            -- claude, qwen, codex, etc.
    agent_source    TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    session_id      TEXT,                     -- backend session ID
    session_status  TEXT NOT NULL DEFAULT 'idle',  -- idle, running, suspended
    session_config  TEXT NOT NULL DEFAULT '{}',     -- JSON config
    last_active_at  INTEGER,
    suspended_at    INTEGER
);
```

### oauth_tokens

```sql
CREATE TABLE oauth_tokens (
    server_url    TEXT PRIMARY KEY,
    access_token  TEXT NOT NULL,
    refresh_token TEXT,
    token_type    TEXT NOT NULL DEFAULT 'bearer',
    expires_at    INTEGER,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
```

---

## 11. Concurrency Model

| Component | Mechanism | Purpose |
|-----------|-----------|---------|
| `AcpState` | `RwLock<AcpState>` | Thread-safe state reads (many) / writes (few) |
| `session_lock` | `Mutex<()>` | Serialize session/new and send operations |
| `last_activity` | `AtomicI64` | Lock-free timestamp for idle detection |
| `raw_rx` | `Mutex<Option<...>>` | Pre-subscribed receiver, taken exactly once |
| `WorkerTaskManager.tasks` | `DashMap` | Lock-free per-entry concurrent access |
| Event broadcast | `broadcast::channel(256)` | Multi-subscriber event delivery |

---

## 12. Middleware Pipeline

### Message Processing (MessageMiddleware)

```
User message in
    │
    ├── 1. strip_think_tags()
    │      Remove <think>...</think> and <thinking>...</thinking>
    │
    ├── 2. detect_cron_commands()
    │      [CRON_CREATE]...[/CRON_CREATE]
    │      [CRON_LIST]
    │      [CRON_DELETE: id]
    │
    ├── 3. Execute cron commands (if ICronService available)
    │
    └── Output: MiddlewareResult { cleaned_message, display_message, system_responses }
```

---

## 13. Skill Management

The `AcpSkillManager` injects skill context into ACP sessions:

```
AcpSkillManager
├── build_skills_index_text()      # Index of available skills
├── build_system_instructions()     # System prompt with skills
├── prepare_first_message()         # Enrich first message with skill context
├── detect_skill_load_request()     # Parse /skill commands from messages
└── SkillDefinition, SkillIndex     # Skill metadata types
```

Skills are injected during `session/new` via:
- `preset_context` in AcpBuildExtra
- `enabled_skills` list in AcpBuildExtra

---

## 14. Architecture Summary Diagram

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          Client (Desktop App)                           │
│                    REST API + WebSocket                                  │
└──────────────┬──────────────────────────────────┬───────────────────────┘
               │                                  │
         HTTP REST                          WebSocket
               │                                  │
┌──────────────▼──────────────────────────────────▼───────────────────────┐
│                           aionui-app                                    │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │  Middleware Stack: CORS → Security → CSRF → Auth → Handler      │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │  AppServices (centralized DI)                                    │    │
│  │  → WorkerTaskManager (per-conversation agent cache)             │    │
│  │  → AgentFactory (build agent by type)                           │    │
│  │  → BroadcastEventBus (real-time events)                         │    │
│  └─────────────────────────────────────────────────────────────────┘    │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  /api/acp/*              → ACP global management                        │
│  /api/conversations/*/acp/* → Per-conversation session control          │
│  /api/remote-agents/*    → Remote agent CRUD                            │
│  /api/bedrock/*          → AWS Bedrock connection test                  │
│  /api/gemini/*           → Gemini subscription check                    │
│                                                                         │
├─────────────────────────────────────────────────────────────────────────┤
│                       aionui-ai-agent                                   │
│                                                                         │
│  ┌──────────────────────┐  ┌──────────────────────┐                    │
│  │   WorkerTaskManager  │  │   AgentFactory        │                    │
│  │   DashMap<conv_id,   │  │   fn(options) →       │                    │
│  │     AgentHandle>     │──│     AgentHandle       │                    │
│  └──────────┬───────────┘  └──────────┬───────────┘                    │
│             │                         │                                 │
│  ┌──────────▼───────────┐  ┌──────────▼───────────┐                    │
│  │   AcpAgentManager    │  │ RemoteAgentManager   │                    │
│  │   (CLI subprocess)   │  │ (WebSocket)          │                    │
│  │   ┌────────────┐     │  │ ┌────────────┐       │                    │
│  │   │ AcpState   │     │  │ │ Connection │       │                    │
│  │   │ (RwLock)   │     │  │ │ (WS)       │       │                    │
│  │   ├────────────┤     │  │ ├────────────┤       │                    │
│  │   │ EventRelay │     │  │ │ EventRelay │       │                    │
│  │   │ (stdout→   │     │  │ │ (ws→       │       │                    │
│  │   │  broadcast)│     │  │ │  broadcast)│       │                    │
│  │   └────────────┘     │  │ └────────────┘       │                    │
│  └──────────────────────┘  └──────────────────────┘                    │
│             │                         │                                 │
│             ▼                         ▼                                 │
│  ┌──────────────────────────────────────────────────┐                  │
│  │        AgentStreamEvent (24 variants)             │                  │
│  │  → broadcast to WebSocket subscribers             │                  │
│  │  → state updates (session_id, confirmations, etc) │                  │
│  └──────────────────────────────────────────────────┘                  │
│                                                                         │
│  ┌──────────────────────┐  ┌──────────────────────┐                    │
│  │   IdleScanner        │  │   SkillManager       │                    │
│  │   (periodic cleanup) │  │   (context injection)│                    │
│  └──────────────────────┘  └──────────────────────┘                    │
│                                                                         │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐                   │
│  │ CLI Process  │ │ WebSocket    │ │ AWS Bedrock   │                   │
│  │ (subprocess) │ │ (tokio-      │ │ (aws-sdk)     │                   │
│  │              │ │  tungstenite)│ │               │                   │
│  └──────┬───────┘ └──────┬───────┘ └───────┬──────┘                   │
│         │                │                  │                           │
└─────────▼────────────────▼──────────────────▼───────────────────────────┘
    Local CLI          Remote Agent          AWS Bedrock
    (claude, qwen,     Server               Runtime
     codex, ...)       (WebSocket)
```

---

## 15. Key Design Decisions & Tradeoffs

| Decision | Rationale |
|----------|-----------|
| CLI subprocess for local agents | Reuses existing CLI tools (claude, qwen, etc.) without reimplementing their protocols; each runs in isolation |
| WebSocket for remote agents | Bidirectional streaming, suitable for long-lived agent sessions |
| Per-conversation agent cache | DashMap provides lock-free reads; at most one active agent per conversation prevents resource waste |
| Approval memory session-scoped | Balances UX (don't ask repeatedly) with security (doesn't persist across restarts) |
| AES-GCM for sensitive DB fields | Encrypted at rest, key derived from JWT secret |
| Backend-specific resume strategies | Different CLIs have incompatible session management; strategy pattern isolates the differences |
| Broadcast channel (cap 256) for events | Bounded buffer prevents memory leaks; dropped events acceptable (UI reconnects) |
| AtomicI64 for last_activity | Lock-free idle detection; millisecond precision sufficient for cleanup |
| Agent factory as closure | Allows sync caller context (thread scope + block_on) for async repo queries during construction |
