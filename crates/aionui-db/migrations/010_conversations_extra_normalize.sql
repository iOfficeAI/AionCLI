-- Migration 010: Normalize conversations.extra JSON keys from camelCase to snake_case
--
-- Old TypeScript backend stored conversation metadata with camelCase keys:
--   agentName, cliPath, currentModelId, sessionMode, customWorkspace,
--   defaultFiles, cachedConfigOptions, loadedSkills, lastContextLimit,
--   lastTokenUsage, acpSessionConversationId, acpSessionId, acpSessionUpdatedAt
--
-- New Rust backend uses snake_case. This migration renames keys that are simple
-- renames (same data structure, just different key name).
--
-- NOT migrated here (structural changes needed, handled by code-layer aliases):
--   - teamMcpStdioConfig (structure changed: old=spawn config, new=TCP config)
--   - cachedConfigOptions, loadedSkills, lastTokenUsage (runtime caches, stale)

-- Rename agentName → agent_name (where agentName exists and agent_name doesn't)
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.agentName'), '$.agent_name', json_extract(extra, '$.agentName'))
WHERE json_extract(extra, '$.agentName') IS NOT NULL
  AND json_extract(extra, '$.agent_name') IS NULL;

-- Rename cliPath → cli_path
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.cliPath'), '$.cli_path', json_extract(extra, '$.cliPath'))
WHERE json_extract(extra, '$.cliPath') IS NOT NULL
  AND json_extract(extra, '$.cli_path') IS NULL;

-- Rename currentModelId → current_model_id
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.currentModelId'), '$.current_model_id', json_extract(extra, '$.currentModelId'))
WHERE json_extract(extra, '$.currentModelId') IS NOT NULL
  AND json_extract(extra, '$.current_model_id') IS NULL;

-- Rename sessionMode → session_mode
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.sessionMode'), '$.session_mode', json_extract(extra, '$.sessionMode'))
WHERE json_extract(extra, '$.sessionMode') IS NOT NULL
  AND json_extract(extra, '$.session_mode') IS NULL;

-- Rename customWorkspace → custom_workspace
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.customWorkspace'), '$.custom_workspace', json_extract(extra, '$.customWorkspace'))
WHERE json_extract(extra, '$.customWorkspace') IS NOT NULL
  AND json_extract(extra, '$.custom_workspace') IS NULL;

-- Rename defaultFiles → default_files
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.defaultFiles'), '$.default_files', json_extract(extra, '$.defaultFiles'))
WHERE json_extract(extra, '$.defaultFiles') IS NOT NULL
  AND json_extract(extra, '$.default_files') IS NULL;

-- Rename acpSessionConversationId → acp_session_conversation_id
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.acpSessionConversationId'), '$.acp_session_conversation_id', json_extract(extra, '$.acpSessionConversationId'))
WHERE json_extract(extra, '$.acpSessionConversationId') IS NOT NULL;

-- Rename acpSessionId → acp_session_id
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.acpSessionId'), '$.acp_session_id', json_extract(extra, '$.acpSessionId'))
WHERE json_extract(extra, '$.acpSessionId') IS NOT NULL;

-- Rename acpSessionUpdatedAt → acp_session_updated_at
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.acpSessionUpdatedAt'), '$.acp_session_updated_at', json_extract(extra, '$.acpSessionUpdatedAt'))
WHERE json_extract(extra, '$.acpSessionUpdatedAt') IS NOT NULL;

-- Normalize teamId → ensure both teamId and team_id exist for transition period
-- (Guide server reads teamId, frontend reads team_id; keep both until code is unified)
UPDATE conversations
SET extra = json_set(extra, '$.team_id', json_extract(extra, '$.teamId'))
WHERE json_extract(extra, '$.teamId') IS NOT NULL
  AND json_extract(extra, '$.team_id') IS NULL;

-- Rename customAgentId → custom_agent_id
UPDATE conversations
SET extra = json_set(json_remove(extra, '$.customAgentId'), '$.custom_agent_id', json_extract(extra, '$.customAgentId'))
WHERE json_extract(extra, '$.customAgentId') IS NOT NULL
  AND json_extract(extra, '$.custom_agent_id') IS NULL;

-- Clean up stale runtime caches that are no longer meaningful after upgrade
-- (These are session-specific caches that won't be valid after a restart anyway)
UPDATE conversations
SET extra = json_remove(extra, '$.cachedConfigOptions', '$.loadedSkills', '$.lastContextLimit', '$.lastTokenUsage')
WHERE json_extract(extra, '$.cachedConfigOptions') IS NOT NULL
   OR json_extract(extra, '$.loadedSkills') IS NOT NULL;

-- Rename teamMcpStdioConfig → mark as legacy (don't remove, code handles gracefully)
-- The old format {command, args, env} is structurally incompatible with new {binary_path, port, token}
-- Team session restore already handles missing/invalid config by creating a new session
UPDATE conversations
SET extra = json_set(
    json_remove(extra, '$.teamMcpStdioConfig'),
    '$.legacy_team_mcp_stdio_config', json_extract(extra, '$.teamMcpStdioConfig')
)
WHERE json_extract(extra, '$.teamMcpStdioConfig') IS NOT NULL
  AND json_extract(extra, '$.team_mcp_stdio_config') IS NULL;
