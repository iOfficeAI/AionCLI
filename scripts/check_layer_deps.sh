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

# Rule (Phase 5): legacy task-manager / agent-task types must not return.
#
# `IWorkerTaskManager`, `WorkerTaskManagerImpl`, and `AgentInstance` were
# deleted outright; any non-comment occurrence in any crate is a regression.
#
# `IAgentTask` was retained as a `pub(crate)` 4-method internal lifecycle
# trait inside `aionui-ai-agent` (it forwards `status / send_message /
# cancel / kill` from each concrete manager into the manager's
# `IAgentConnector` impl without copy-pasting ~100-line bodies). Outside
# `aionui-ai-agent` no caller may name it; doc-comment / `//` line
# references inside the crate are explicitly tolerated so the historical
# narrative remains readable.
#
# Matching rules:
#   - Source file scope: `*.rs` only; `target/` and `node_modules/`
#     directories are skipped.
#   - Comment-only lines (after leading whitespace, beginning with `//`
#     including `//!` and `///`) are NOT considered violations. This
#     covers every legitimate residual mention, e.g. `connector_factory.rs`
#     header notes that the factory `replaces IWorkerTaskManager`.
#   - Anywhere a token appears as a real code identifier (`use`, type
#     position, trait bound, etc.) the line will not start with `//` and
#     therefore the rule fires.

global_forbidden=("IWorkerTaskManager" "WorkerTaskManagerImpl" "AgentInstance")
ai_agent_internal_forbidden=("IAgentTask")

# Strip comment-only lines (but keep the line numbers in the report) by
# matching the negation pattern with grep -nP. We use the lookahead-free
# alternative: `grep -nE '\\bTOKEN\\b'` then post-filter with awk.
check_token() {
    local token="$1"
    local exclude_path_prefix="${2:-}"
    while IFS=: read -r file lineno content; do
        # Skip the configured exclusion (used for IAgentTask inside ai-agent).
        if [[ -n "$exclude_path_prefix" && "$file" == "$exclude_path_prefix"* ]]; then
            continue
        fi
        # Skip comment-only lines (`//`, `///`, `//!`, with optional leading
        # whitespace).
        if [[ "$content" =~ ^[[:space:]]*// ]]; then
            continue
        fi
        violations+=("$file:$lineno: forbidden Phase-5 legacy token '$token'")
    done < <(
        grep -RInE \
            --include='*.rs' \
            --exclude-dir=target \
            --exclude-dir=node_modules \
            "\\b${token}\\b" \
            crates/ 2>/dev/null || true
    )
}

for token in "${global_forbidden[@]}"; do
    check_token "$token"
done

for token in "${ai_agent_internal_forbidden[@]}"; do
    check_token "$token" "crates/aionui-ai-agent/"
done

# Rule: connect-layer public connector surface must not expose
# conversation-runtime vocabulary. `ConvActor` / `IConversationService`
# are the runtime source of truth; connectors speak protocol/process.
if grep -RInE \
    --include='*.rs' \
    "\\bConversationStatus\\b" \
    crates/aionui-ai-agent/src/connector 2>/dev/null; then
    violations+=("crates/aionui-ai-agent/src/connector: forbidden ConversationStatus in connector surface")
fi

if (( ${#violations[@]} > 0 )); then
    echo "Layer-dep violations:" >&2
    for v in "${violations[@]}"; do
        echo "   $v" >&2
    done
    exit 1
fi

echo "Layer-dep check passed."
