-- Migration 002: Normalize legacy data from the pre-split TypeScript era
--
-- When aionui-backend.db is created by copying the Electron-managed aionui.db,
-- the data still uses the old formats (camelCase JSON keys, array model fields,
-- empty acp_session table). This migration brings all legacy data to the format
-- expected by the Rust backend.
--
-- Safe to run on fresh databases (all statements are conditional / idempotent).

------------------------------------------------------------------------
-- Part A: Normalize conversations.extra JSON keys (camelCase → snake_case)
--
-- NOTE: Missing columns (pinned, pinned_at, agents_version, etc.) are
-- handled by ensure_schema_columns() in database.rs which runs before
-- migrations. This migration only performs data transformations.
------------------------------------------------------------------------

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.agentName'), '$.agent_name', json_extract(extra, '$.agentName'))
WHERE json_extract(extra, '$.agentName') IS NOT NULL
  AND json_extract(extra, '$.agent_name') IS NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.cliPath'), '$.cli_path', json_extract(extra, '$.cliPath'))
WHERE json_extract(extra, '$.cliPath') IS NOT NULL
  AND json_extract(extra, '$.cli_path') IS NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.currentModelId'), '$.current_model_id', json_extract(extra, '$.currentModelId'))
WHERE json_extract(extra, '$.currentModelId') IS NOT NULL
  AND json_extract(extra, '$.current_model_id') IS NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.sessionMode'), '$.session_mode', json_extract(extra, '$.sessionMode'))
WHERE json_extract(extra, '$.sessionMode') IS NOT NULL
  AND json_extract(extra, '$.session_mode') IS NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.customWorkspace'), '$.custom_workspace', json_extract(extra, '$.customWorkspace'))
WHERE json_extract(extra, '$.customWorkspace') IS NOT NULL
  AND json_extract(extra, '$.custom_workspace') IS NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.defaultFiles'), '$.default_files', json_extract(extra, '$.defaultFiles'))
WHERE json_extract(extra, '$.defaultFiles') IS NOT NULL
  AND json_extract(extra, '$.default_files') IS NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.acpSessionConversationId'), '$.acp_session_conversation_id', json_extract(extra, '$.acpSessionConversationId'))
WHERE json_extract(extra, '$.acpSessionConversationId') IS NOT NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.acpSessionId'), '$.acp_session_id', json_extract(extra, '$.acpSessionId'))
WHERE json_extract(extra, '$.acpSessionId') IS NOT NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.acpSessionUpdatedAt'), '$.acp_session_updated_at', json_extract(extra, '$.acpSessionUpdatedAt'))
WHERE json_extract(extra, '$.acpSessionUpdatedAt') IS NOT NULL;

UPDATE conversations
SET extra = json_set(extra, '$.team_id', json_extract(extra, '$.teamId'))
WHERE json_extract(extra, '$.teamId') IS NOT NULL
  AND json_extract(extra, '$.team_id') IS NULL;

UPDATE conversations
SET extra = json_set(json_remove(extra, '$.customAgentId'), '$.custom_agent_id', json_extract(extra, '$.customAgentId'))
WHERE json_extract(extra, '$.customAgentId') IS NOT NULL
  AND json_extract(extra, '$.custom_agent_id') IS NULL;

-- Clean up stale runtime caches
UPDATE conversations
SET extra = json_remove(extra, '$.cachedConfigOptions', '$.loadedSkills', '$.lastContextLimit', '$.lastTokenUsage')
WHERE json_extract(extra, '$.cachedConfigOptions') IS NOT NULL
   OR json_extract(extra, '$.loadedSkills') IS NOT NULL;

-- Rename legacy teamMcpStdioConfig → legacy_team_mcp_stdio_config
UPDATE conversations
SET extra = json_set(
    json_remove(extra, '$.teamMcpStdioConfig'),
    '$.legacy_team_mcp_stdio_config', json_extract(extra, '$.teamMcpStdioConfig')
)
WHERE json_extract(extra, '$.teamMcpStdioConfig') IS NOT NULL
  AND json_extract(extra, '$.legacy_team_mcp_stdio_config') IS NULL;

------------------------------------------------------------------------
-- Part B: Normalize conversations.model from legacy provider format
--
-- Legacy: {"id":"xxx", "model":["gpt-5.2","gpt-4o"], "useModel":"gpt-5.2", ...}
-- Target: {"provider_id":"xxx", "model":"gpt-5.2", "use_model":null}
------------------------------------------------------------------------

UPDATE conversations
SET model = json_object(
    'provider_id', json_extract(model, '$.id'),
    'model',       json_extract(model, '$.useModel'),
    'use_model',   NULL
)
WHERE model IS NOT NULL
  AND json_valid(model)
  AND json_type(model, '$.model') = 'array'
  AND json_extract(model, '$.useModel') IS NOT NULL;

------------------------------------------------------------------------
-- Part C: Normalize teams.agents JSON (camelCase → snake_case)
--
-- Only runs on teams with agents_version = '1.0.0' (pre-normalization).
-- After conversion sets agents_version = '1.0.1'.
------------------------------------------------------------------------

UPDATE teams
SET agents = (
    SELECT json_group_array(
        json_object(
            'slot_id',           json_extract(value, '$.slotId'),
            'name',              COALESCE(json_extract(value, '$.agentName'), json_extract(value, '$.name'), ''),
            'role',              CASE
                                   WHEN COALESCE(json_extract(value, '$.role'), '') IN ('lead', 'leader') THEN 'lead'
                                   ELSE 'teammate'
                                 END,
            'conversation_id',   COALESCE(json_extract(value, '$.conversationId'), json_extract(value, '$.conversation_id'), ''),
            'backend',           COALESCE(json_extract(value, '$.agentType'), json_extract(value, '$.backend'), ''),
            'model',             COALESCE(json_extract(value, '$.model'), ''),
            'status',            COALESCE(json_extract(value, '$.status'), 'pending'),
            'conversation_type', COALESCE(json_extract(value, '$.conversationType'), json_extract(value, '$.conversation_type'), ''),
            'cli_path',          json_extract(value, '$.cliPath'),
            'custom_agent_id',   json_extract(value, '$.customAgentId')
        )
    )
    FROM json_each(teams.agents)
),
agents_version = '1.0.1'
WHERE agents_version = '1.0.0'
  AND json_valid(agents)
  AND json_array_length(agents) > 0
  AND json_extract(agents, '$[0].slotId') IS NOT NULL;

-- Teams with empty agents arrays also get marked as normalized
UPDATE teams
SET agents_version = '1.0.1'
WHERE agents_version = '1.0.0'
  AND (agents = '[]' OR json_array_length(agents) = 0);

------------------------------------------------------------------------
-- Part D: Backfill acp_session rows from conversations.extra
------------------------------------------------------------------------

INSERT OR IGNORE INTO acp_session (
    conversation_id,
    agent_backend,
    agent_source,
    agent_id,
    session_id,
    session_status,
    session_config
)
SELECT
    c.id,
    COALESCE(json_extract(c.extra, '$.backend'), ''),
    'builtin',
    '',
    json_extract(c.extra, '$.acp_session_id'),
    'idle',
    '{}'
FROM conversations c
WHERE c.type = 'acp'
  AND json_extract(c.extra, '$.acp_session_id') IS NOT NULL
  AND c.id NOT IN (SELECT conversation_id FROM acp_session);
