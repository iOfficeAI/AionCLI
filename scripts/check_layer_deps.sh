#!/usr/bin/env bash
# Enforce the four-layer dependency rule.
#
#   biz layer (team / cron / channel)  -->  conv (aionui-conversation)
#                                      -->  connect (aionui-ai-agent) [allowed only as transitive]
#                                      -->  agent  (aionrs etc.)
#
# Rules enforced as of Phase 3:
#   - aionui-team source's hot-path biz-layer entry points (event_loop.rs
#     and session.rs) MUST NOT import the connect-layer task-manager
#     surface (`IWorkerTaskManager`, `IAgentTask`, `AgentInstance`,
#     `WorkerTaskManagerImpl`, `BuildTaskOptions`, `SendMessageData`).
#     These are connect-layer types that the conv layer's
#     `IConversationService` already wraps.
#
# The carve-outs:
#   - `aionui-team`'s `service.rs` still wires `IWorkerTaskManager` for
#     `set_session_mode` and process-rebuild on `team_mcp_stdio_config`
#     update. Phase 5 surfaces a process-rebuild trait method on
#     `IConversationService` and removes that dep.
#   - `aionui-cron` and `aionui-channel` may still import the connect
#     layer until Phase 4 / Phase 5 finishes their migration.
#   - `aionui-team` tests under `tests/` and `#[cfg(test)]` blocks may
#     still import connect-layer types until Phase 5 deletes
#     `IWorkerTaskManager` outright.
#
# Failure => exit 1 with a human message; success => silent exit 0.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

violations=()

# Rule: no connect-layer task-manager / agent-task surface inside
# the Phase-3 hot-path biz-layer entry points. Specifically:
#   - crates/aionui-team/src/event_loop.rs   (per-agent event loop)
#   - crates/aionui-team/src/session.rs       (session wakeup + spawn)
#
# `crates/aionui-team/src/service.rs` and `service/spawn_support.rs`
# legitimately keep `IWorkerTaskManager` for `set_session_mode` and
# process-rebuild on team_mcp_stdio_config updates; Phase 5 surfaces
# a process-rebuild trait method on `IConversationService` and removes
# that dep.
#
# Inline `#[cfg(test)]` blocks inside the gated files are excluded
# (truncated at the first top-level `#[cfg(test)]`).
forbidden_tokens=(
    "IWorkerTaskManager"
    "WorkerTaskManagerImpl"
    "IAgentTask"
    "AgentInstance"
    "BuildTaskOptions"
    "SendMessageData"
)

gated_files=(
    "crates/aionui-team/src/event_loop.rs"
    "crates/aionui-team/src/session.rs"
)

for path in "${gated_files[@]}"; do
    [[ -f "$path" ]] || continue
    # Truncate at first top-level `#[cfg(test)]` so inline test modules
    # are excluded.
    awk '/^#\[cfg\(test\)\]/{exit} {print}' "$path" > /tmp/.layer_check_filtered
    for token in "${forbidden_tokens[@]}"; do
        if grep -qE "\\b${token}\\b" /tmp/.layer_check_filtered; then
            violations+=("$path: forbidden connect-layer token '$token' in non-test code")
        fi
    done
done
rm -f /tmp/.layer_check_filtered

if (( ${#violations[@]} > 0 )); then
    echo "Layer-dep violations:" >&2
    for v in "${violations[@]}"; do
        echo "   $v" >&2
    done
    exit 1
fi

echo "Layer-dep check passed."
