# AGENTS — `aionui-ai-agent` (connect layer)

This crate is the **connect layer** in the four-layer architecture
(see root `AGENTS.md` and `ARCHITECTURE.md`). It speaks protocol and
process — never conversation runtime state.

## Hard rules

### 1. No conv-layer types in the connect-layer surface

The public trait `IAgentConnector` (in `src/connector/`) MUST NOT mention
`ConversationStatus`, `ConversationId`, conversation events, or any other
conv-layer type. If you find yourself wanting to import
`aionui_common::ConversationStatus` into a connector trait, stop and route
the data through `ConnectorEvent` / `TurnSummary` instead.

CI grep: `rg "ConversationStatus" crates/aionui-ai-agent/src/connector` MUST return zero matches.

### 2. `cancel_current_turn` MUST NOT return until protocol-acknowledged

This is the structural fix for ELECTRON-1KB. Implementations of
`IAgentConnector::cancel_current_turn` may not return `Ok(())` until:

- ACP: the SDK's `prompt()` future has resolved (cancel-ack notify fires)
  OR a 5s safety timeout elapses;
- aionrs: the `turn_done` oneshot receiver has signalled;
- Remote: a `Finish` (or `Error`) event has been observed on the legacy
  stream OR a 5s safety timeout elapses.

Returning early from `cancel_current_turn` reintroduces the cancel→send
race. Reviewers MUST reject any change that shortcuts this.

### 3. `run_turn` MUST be single-flight per connector instance

Concurrent callers of `run_turn` on the same connector MUST be serialized
or one MUST observe `ConnectorError::Busy`. This is defense in depth — the
conv-layer mutex (Phase 2) is the primary serializer; this rule prevents a
buggy biz-layer caller from creating overlapping turns at the protocol
level.

### 4. Existing rule preserved: do not add fields to `AcpAgentManager`

See root `AGENTS.md` § "Do NOT add fields to `AcpAgentManager`" — that
rule continues to apply. The Phase 1 `cancel_ack: Arc<Notify>` field is
the documented exception (turn-lifecycle, transient, not derivable from
existing state).

## Module layout

| Module | Responsibility |
|---|---|
| `src/connector/` | `IAgentConnector` trait, `ConnectorEvent` types, errors |
| `src/connector_factory.rs` | `IAgentConnectorFactory` + default `ConnectorFactory` cache (Phase 5) |
| `src/manager/{acp,aionrs,remote,nanobot,openclaw}` | Per-protocol implementations of `IAgentConnector` |
| `src/agent_task.rs` | Crate-private `IAgentTask` shared base — **no longer re-exported from `lib.rs` (Phase 5)**; production callers use `IAgentConnector` |
| `src/task_manager.rs` | **Deleted in Phase 5.** Legacy `IWorkerTaskManager` and `WorkerTaskManagerImpl` — replaced by `IAgentConnectorFactory` |
| `src/idle_scanner.rs` | **Relocated to conv layer in Phase 5.** See `crates/aionui-conversation/src/idle_scanner.rs` |

## Test obligations for new connector implementations

Every new `IAgentConnector` impl MUST add tests covering:

1. `cancel_current_turn` blocks until the protocol acks the stop.
2. Concurrent `run_turn` returns `Busy` to one caller (or serializes).
3. `cancel_current_turn` is idempotent when no turn is in flight.

## What this crate may depend on

Allowed:
- `aionui-common`, `aionui-db`, `aionui-runtime`, `aionui-auth`,
  `aionui-extension`, `aionui-system`, `aionui-api-types`
- `agent-client-protocol`, `aion-agent`, `aion-types`, `aion-protocol`,
  `aion-config`, `aion-mcp`
- Standard tokio / serde / tracing crates

Forbidden:
- Any biz-layer crate (`aionui-team`, `aionui-cron`, `aionui-assistant`)
- The conv-layer crate (`aionui-conversation`)

Phase 3 wired `scripts/check_layer_deps.sh` into the `just push` gate;
it greps for forbidden cross-layer imports and the Phase-5-deleted
symbols (`IWorkerTaskManager`, `WorkerTaskManagerImpl`, `AgentInstance`,
`IMockAgent`).
