# AGENTS — `aionui-conversation` (conv layer)

Conv layer in the four-layer architecture. Owns conversation runtime
state via `ConvActor`. Composes connect-layer connectors. Used by
biz-layer crates exclusively through `IConversationService`.

## Hard rules

### 1. ConvActor is the single source of truth for runtime state

Runtime status (Idle / Running) lives in `ConvActor::state`. Do NOT:

- Read `DB.conversations.status` to decide if a conversation is running.
- Write `DB.conversations.status` from runtime hot paths (`send` /
  `cancel` / `complete_conversation`).
- Cache the runtime status in any other structure.

CI grep:

```bash
rg "status: Some\\(.*ConversationStatus::Running" crates/aionui-conversation/src
```

MUST return zero matches.

### 2. Turn serialization happens inside ConvActor's mutex

If you find yourself wanting to add another mutex / `OnceCell` /
`AtomicBool` to track turn state, stop. The
`ConvState::Running { msg_id, turn_done }` mutex is the single
serializer. Multi-turn chaining (cron continuations etc.) is a
biz-layer concern — see `aionui-cron` (Phase 4).

### 3. `cancel()` MUST wait for `wait_for_idle()`

`IConversationService::cancel` returns only after
`actor.wait_for_idle()` has settled. This depends on Phase 1's
`connector.cancel_current_turn()` contract being honoured by the
underlying `IAgentConnector` (i.e. the connector must release its
slot when cancelled). Returning early reintroduces the cancel→send
race that produced ELECTRON-1KB.

Regression test: `tests/cancel_send_race.rs::cancel_then_send_does_not_return_conflict`.

### 4. No biz-layer types in conv-layer surface

`IConversationService` MUST NOT mention team / cron / assistant types.
Biz-layer orchestration lives outside this crate. If a new conv-layer
method needs biz context, expose a hook instead and let the biz layer
register a callback.

### 5. `send` is single-turn

`IConversationService::send` dispatches exactly one turn and emits
`ConversationEvent::TurnCompleted { msg_id, system_responses }` when
the relay finishes. Multi-turn chaining (cron continuations,
agent-self-prompted follow-ups, etc.) is a biz-layer concern —
`aionui-cron`'s `CronContinuationOrchestrator` is the canonical
subscriber. Do NOT add a continuation loop, retry loop, or pending-send
queue inside the conv-layer spawn task.

CI grep:

```bash
rg "MAX_.*CONTINUATIONS|continuation_count|pending_send" crates/aionui-conversation/src
```

MUST return zero matches.

### 6. The conv layer owns idle-conversation cleanup

Idle decision logic lives in `aionui-conversation`. Specifically:

- `start_idle_scanner` — the periodic background task that walks the
  active actor map — lives in `crates/aionui-conversation/src/idle_scanner.rs`.
- The "idle for ≥ N seconds AND `ConvState::Idle`" predicate is
  `IConversationService::collect_idle`, paired with
  `IConversationService::cancel_idle` which performs the actual
  shutdown.
- The scanner consumes `Arc<dyn IConversationService>`; it must NOT
  reach into the connect layer to ask agents about their own idle
  status.

The connect-layer `IAgentConnectorFactory` (in `aionui-ai-agent`)
intentionally exposes no idle-policy hooks. If the connector needs to
participate in shutdown, the conv layer drives it through
`IConversationService::cancel_idle`, which already calls
`connector.kill(reason = IdleTimeout)` under the hood.

## Module layout

| Module | Responsibility |
|---|---|
| `conv_service_trait.rs` | `IConversationService`, `ConversationStatus`, `ConversationEvent` |
| `conv_actor.rs` | `ConvActor`, `ConvState`, `TurnHandle` |
| `service.rs` | `ConversationService` impl (orchestration, repo, hooks) |
| `stream_relay.rs` | Event fan-out from connector → DB → WebSocket |
| `convert.rs` | DB row ↔ API response mapping |
| `routes.rs` / `routes_aux.rs` | HTTP handlers — request/response only |
| `state.rs` | `ConversationRouterState` |
| `response_middleware.rs` | Cron + `<think>` post-processing |
| `skill_resolver.rs` / `skill_snapshot.rs` | Extension/skill plumbing |
| `task_options.rs` | Build options forwarded to connect-layer connector factory |
| `idle_scanner.rs` | `start_idle_scanner` background task (see Hard rule 6) |

## Allowed dependencies

- `aionui-common`, `aionui-db`, `aionui-realtime`, `aionui-api-types`
- `aionui-ai-agent` (connect layer) — through `IAgentConnectorFactory`
  + `Arc<dyn IAgentConnector>` ONLY. The legacy task-manager surface
  (`IWorkerTaskManager`, `AgentInstance`, `IAgentTask`,
  `WorkerTaskManagerImpl`) was deleted in Phase 5; reintroducing any of
  those names is blocked by `scripts/check_layer_deps.sh`.

## Forbidden dependencies

- `aionui-team`, `aionui-cron`, `aionui-assistant` (biz layer)
- Any HTTP framework type in trait surfaces (axum types belong to
  routes only)

## Testing

- New trait surface methods (`IConversationService::*`) require
  trait-surface tests in `tests/`.
- Cancel/send race regressions live in `tests/cancel_send_race.rs`.
  Add a new case there for any change that touches the actor mutex,
  `TurnHandle::Drop`, or `cancel()`'s wait sequencing.
- Database tests use `init_database_memory()`; do not introduce
  on-disk SQLite paths in the test setup.

## Logging

- `info` for lifecycle boundaries (conversation created / turn
  started / cancel issued).
- `warn` for malformed data, repository errors that are
  individually recoverable (e.g. failed message persistence inside a
  spawned turn).
- `error` for contract violations (e.g. actor reaching `Running`
  without a `msg_id`).
- NEVER log raw user content, tool input/output, agent prompt
  payloads, or secrets at any level visible in production builds.
