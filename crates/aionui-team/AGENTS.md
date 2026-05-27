# AGENTS — `aionui-team` (biz layer)

Biz layer in the four-layer architecture. Owns team session state,
mailbox + scheduler, and the per-agent event loop. Reaches the
connect layer ONLY through the conv layer's `IConversationService`.

## Hard rules

### 1. Biz hot paths go through `IConversationService`

The team event loop (`event_loop.rs`) and the inline-wake fallback
(`session.rs::try_wake_inline`) MUST dispatch turns through
`IConversationService::warmup` + `status` + `send`. Do NOT:

- Import `IWorkerTaskManager`, `IAgentTask`, `AgentInstance`,
  `BuildTaskOptions` or `SendMessageData` in `event_loop.rs` /
  `session.rs`.
- Read `DB.conversations.status` to decide whether a turn is running —
  use `IConversationService::status` (lock-free) instead.
- Write `DB.conversations.status` from biz-layer hot paths. The
  conv-layer `ConvActor` + `StreamRelay` are the single writer.

CI gate: `scripts/check_layer_deps.sh` (wired into `just push`)
greps the forbidden tokens out of `event_loop.rs` and `session.rs`.

### 2. `service.rs` carve-out

`TeamSessionService` (in `service.rs` and `service/spawn_support.rs`)
keeps an `Arc<dyn IWorkerTaskManager>` for two specific paths:

- `set_session_mode` — pushes mode changes into the live connector
  via `instance.set_mode(mode)`.
- `attach_spawned_agent_process_bg` — kills the connector with
  `AgentKillReason::TeamMcpRebuild` after a `team_mcp_stdio_config`
  update, then calls `IConversationService::warmup` to rebuild.

These will move to a process-rebuild trait method on
`IConversationService` in Phase 5. Until then, NEW call sites in
service.rs MUST be justified inline (doc comment on the method)
explaining why the conv-layer trait can't carry the call. The
layer-check script gates `event_loop.rs` and `session.rs` only;
service.rs must self-police.

### 3. `remove_agent` does not kill processes

`TeamSession::remove_agent` no longer calls `task_manager.kill`.
Process teardown is the upstream
`TeamSessionService::remove_agent`'s job — it calls
`IConversationService::delete`, which fires the
`task_manager_delete_hook`. Adding a kill back into the biz layer
is forbidden; it would either be redundant after delete or reach
into the connect layer the layer-check rejects.

### 4. Logging hygiene (mirrors AGENTS.md root)

Production-visible logs MUST NOT include prompts, tool input/output,
file contents, secrets, or raw provider responses. The wake path
specifically logs only the `team_id` / `slot_id` /
`conversation_id` / `error` triplet — never the message body.

## Module layout

| Module | Responsibility |
|---|---|
| `event_loop.rs` | Per-agent event loop. Wakes through `IConversationService`. |
| `session.rs` | `TeamSession` — scheduler/mailbox/MCP server holder, inline-wake fallback, spawn flow. |
| `service.rs` | `TeamSessionService` — DB orchestration, ensure_session, set_session_mode, remove_agent. |
| `service/spawn_support.rs` | Spawn-flow helpers: `parse_agent_type`, `persist_spawned_agent`, `attach_spawned_agent_process_bg`. |
| `scheduler/` | `TeammateManager` — slot status + wake locks. |
| `mailbox.rs` | Per-team unread queue. |
| `task_board.rs` | Lead-managed task list. |
| `mcp/` | `TeamMcpServer` — TCP MCP endpoint that spawned agents connect to. |
| `prompts.rs` | Lead/teammate role-prompt builders. |
| `routes.rs` / `state.rs` | HTTP handlers (request/response only). |

Each module file is single-responsibility. Files approaching 1000
lines must be split — `session.rs` is currently above the budget
(legacy growth) and any new feature MUST add to a new submodule, not
extend `session.rs`.

## Allowed dependencies

- `aionui-common`, `aionui-db`, `aionui-realtime`, `aionui-api-types`
- `aionui-conversation` — through `IConversationService` (preferred)
  and `ConversationService` for the spawn-only `update_extra` /
  `insert_raw_message` paths.
- `aionui-ai-agent` — restricted to `service.rs` and
  `service/spawn_support.rs` (carve-out above). New call sites in
  `event_loop.rs` / `session.rs` are layer-check failures.

## Forbidden dependencies

- `aionui-cron`, `aionui-assistant` (cross-biz-layer reach-arounds)
- Any direct import of axum / tower in service.rs (HTTP types live
  in routes.rs only).

## Testing

- Unit tests in `session.rs` use a `MockConvService` implementing
  `IConversationService`. Adding new test scaffolding that mints
  `AgentInstance::Mock` is a Phase-3 regression — extend
  `MockConvService` instead.
- The e2e flow tests (`tests/e2e_team_flow.rs`) use the same mock
  shape; their `SendCall` capture matches the trait `send` signature.
- New scheduler / mailbox / task-board behavior must add a happy-path
  + bad-path integration test under `tests/`.
- Spawn-path tests in `session.rs` use a `Weak<TeamSessionService>`
  null sentinel and assert that the validation chain returns
  `InvalidRequest("...live TeamSessionService...")` once the DB step
  is reached. Do NOT short-circuit those guards in tests by giving
  the session a fake live service — tests must exercise validation
  through to the documented stop point.

## Layer-dep check

Run locally:

```bash
just layer-check
```

Equivalent to:

```bash
bash scripts/check_layer_deps.sh
```

Wired into `just push` so divergent commits cannot reach origin.
