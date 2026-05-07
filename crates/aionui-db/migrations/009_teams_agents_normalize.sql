-- Migration 009: Normalize teams.agents JSON from camelCase to snake_case
--
-- Old TypeScript backend stored team agents with camelCase field names:
--   slotId, conversationId, agentType, agentName, conversationType, cliPath
-- New Rust backend expects snake_case:
--   slot_id, conversation_id, backend, name, conversation_type, cli_path
--
-- This migration converts all existing agents JSON in-place using SQLite JSON
-- functions. COALESCE handles both old (camelCase) and already-new (snake_case)
-- data, making the migration idempotent.

-- Step 1: Add version tracking column for future migrations
ALTER TABLE teams ADD COLUMN agents_version TEXT NOT NULL DEFAULT '1.0.0';

-- Step 2: Transform non-empty agents arrays
UPDATE teams
SET agents = (
    SELECT json_group_array(
        json_object(
            'slot_id',           COALESCE(json_extract(j.value, '$.slotId'), json_extract(j.value, '$.slot_id'), ''),
            'name',              COALESCE(json_extract(j.value, '$.agentName'), json_extract(j.value, '$.name'), ''),
            'role',              CASE json_extract(j.value, '$.role')
                                   WHEN 'leader' THEN 'lead'
                                   ELSE COALESCE(json_extract(j.value, '$.role'), 'teammate')
                                 END,
            'conversation_id',   COALESCE(json_extract(j.value, '$.conversationId'), json_extract(j.value, '$.conversation_id'), ''),
            'backend',           COALESCE(json_extract(j.value, '$.agentType'), json_extract(j.value, '$.backend'), ''),
            'model',             COALESCE(json_extract(j.value, '$.model'), ''),
            'status',            json_extract(j.value, '$.status'),
            'conversation_type', COALESCE(json_extract(j.value, '$.conversationType'), json_extract(j.value, '$.conversation_type')),
            'cli_path',          COALESCE(json_extract(j.value, '$.cliPath'), json_extract(j.value, '$.cli_path')),
            'custom_agent_id',   COALESCE(json_extract(j.value, '$.customAgentId'), json_extract(j.value, '$.custom_agent_id'))
        )
    ) FROM json_each(teams.agents) AS j
),
agents_version = '1.0.1'
WHERE agents != '[]';

-- Step 3: Mark empty arrays as already compliant
UPDATE teams SET agents_version = '1.0.1' WHERE agents = '[]';
