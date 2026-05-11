-- Migration 014: Backfill legacy data from pre-split (TypeScript) era
--
-- Fixes two issues for databases copied from the old aionui.db:
--
-- 1. conversations.model column stored the full provider object with
--    "model" as an array of available models. The Rust backend expects
--    the flat ProviderWithModel format: {provider_id, model, use_model}.
--
-- 2. acp_session table was empty in the old DB. Session IDs were stored
--    only in conversations.extra.acp_session_id. Without a corresponding
--    acp_session row the backend cannot resume sessions.

------------------------------------------------------------------------
-- Part A: Normalize conversations.model from legacy provider format
--         to the flat ProviderWithModel format.
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
-- Part B: Backfill acp_session rows from conversations.extra for
--         historical ACP conversations that have a session_id in extra
--         but no corresponding acp_session row.
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
