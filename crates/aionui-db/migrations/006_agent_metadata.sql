-- Agent metadata catalog.
--
-- Replaces the static `AcpBackend` enum and becomes the single source of
-- truth for how each agent is spawned. Seeds the 17 ACP vendors, two
-- non-ACP builtins (nanobot, openclaw-gateway), and one internal (aionrs).
--
-- Row semantics:
--   command             Program-name form that the adapter resolves via `which()`
--                       at spawn time. For bridge-based ACP vendors this is
--                       "bun"; for direct-CLI vendors it is the CLI itself.
--   args                JSON string[] passed verbatim after the resolved command.
--   env                 JSON array of {name,value,description?} entries.
--   native_skills_dirs  JSON string[] or NULL (NULL → prompt-injection).
--   agent_source_info   JSON: {"binary_name":"...",
--                              "bridge_binary":"bun"|null,
--                              "hub_package_id":..., "version":...}
--                       `binary_name` is the primary CLI checked for availability.
--                       `bridge_binary` is an additional binary required for
--                       bridge-based ACP spawns (e.g. "bun").
--   behavior_policy     JSON: adapter-side behaviour flags. Current keys:
--                         "supports_side_question": bool
--                       Whether the agent supports session/load is read
--                       from the handshake's
--                       `agent_capabilities.load_session` bool, not from
--                       this column.
--   yolo_id             Native mode id that AionUi's legacy `yolo` /
--                       `yoloNoSandbox` aliases resolve to before
--                       calling `session/set_mode`. NULL means the
--                       backend has no yolo equivalent and the alias
--                       should pass through unchanged.
--   agent_capabilities / auth_methods / config_options /
--   available_modes / available_models / available_commands:
--     raw JSON captured from the ACP handshake, refreshed per init.

CREATE TABLE IF NOT EXISTS agent_metadata (
    id                  TEXT PRIMARY KEY NOT NULL,
    icon                TEXT,
    name                TEXT NOT NULL,
    name_i18n           TEXT,
    description         TEXT,
    description_i18n    TEXT,

    backend             TEXT,
    agent_type          TEXT NOT NULL,
    agent_source        TEXT NOT NULL,
    agent_source_info   TEXT,

    enabled             INTEGER NOT NULL DEFAULT 1,

    command             TEXT,
    args                TEXT,
    env                 TEXT,
    native_skills_dirs  TEXT,

    behavior_policy     TEXT,
    yolo_id             TEXT,

    agent_capabilities  TEXT,
    auth_methods        TEXT,
    config_options      TEXT,
    available_modes     TEXT,
    available_models    TEXT,
    available_commands  TEXT,

    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,

    UNIQUE(agent_source, name)
);

CREATE INDEX IF NOT EXISTS idx_agent_metadata_backend    ON agent_metadata(backend);
CREATE INDEX IF NOT EXISTS idx_agent_metadata_agent_type ON agent_metadata(agent_type);

-- ── Seed: ACP vendors (agent_source='builtin', agent_type='acp') ─────
-- ids are fnv1a_hex8("{agent_source}:{name}") computed offline so the
-- seed stays deterministic and idempotent across environments.

INSERT OR IGNORE INTO agent_metadata
    (id, icon, name, name_i18n, description, description_i18n,
     backend, agent_type, agent_source, agent_source_info,
     enabled, command, args, env, native_skills_dirs, behavior_policy, yolo_id,
     agent_capabilities, auth_methods, config_options,
     available_modes, available_models, available_commands,
     created_at, updated_at)
VALUES
    ('2d23ff1c', NULL, 'Claude', NULL, NULL, NULL,
     'claude', 'acp', 'builtin',
     '{"binary_name":"claude","bridge_binary":"bun"}',
     1, 'bun',
     '["x","--bun","@agentclientprotocol/claude-agent-acp@0.29.2"]',
     '[]',
     '[".claude/skills"]',
     '{"supports_side_question":true}',
     'bypassPermissions',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('8e1acf31', NULL, 'Codex', NULL, NULL, NULL,
     'codex', 'acp', 'builtin',
     '{"binary_name":"codex","bridge_binary":"bun"}',
     1, 'bun',
     '["x","--bun","@zed-industries/codex-acp@0.9.5"]',
     '[]',
     '[".codex/skills"]',
     '{"supports_side_question":false}',
     'full-access',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('cc126dd5', NULL, 'Gemini', NULL, NULL, NULL,
     'gemini', 'acp', 'builtin',
     '{"binary_name":"gemini"}',
     1, 'gemini', '["--experimental-acp"]', '[]',
     '[".gemini/skills"]',
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('26a946ed', NULL, 'Qwen', NULL, NULL, NULL,
     'qwen', 'acp', 'builtin',
     '{"binary_name":"qwen"}',
     1, 'qwen', '["--acp"]', '[]',
     '[".qwen/skills"]',
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),


    ('8b20fd41', NULL, 'CodeBuddy', NULL, NULL, NULL,
     'codebuddy', 'acp', 'builtin',
     '{"binary_name":"codebuddy","bridge_binary":"bun"}',
     1, 'bun',
     '["x","--bun","@tencent-ai/codebuddy-code@2.73.0","--acp"]',
     '[]',
     '[".codebuddy/skills"]',
     '{"supports_side_question":false}',
     'bypassPermissions',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('da386544', NULL, 'Droid', NULL, NULL, NULL,
     'droid', 'acp', 'builtin',
     '{"binary_name":"droid"}',
     1, 'droid', '["exec","--output-format","acp"]', '[]',
     '[".factory/skills"]',
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('600c6601', NULL, 'Goose', NULL, NULL, NULL,
     'goose', 'acp', 'builtin',
     '{"binary_name":"goose"}',
     1, 'goose', '["acp"]', '[]',
     '[".goose/skills"]',
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('eb895030', NULL, 'Auggie', NULL, NULL, NULL,
     'auggie', 'acp', 'builtin',
     '{"binary_name":"auggie"}',
     1, 'auggie', '["--acp"]', '[]',
     NULL,
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('e241c49c', NULL, 'Kimi', NULL, NULL, NULL,
     'kimi', 'acp', 'builtin',
     '{"binary_name":"kimi"}',
     1, 'kimi', '["acp"]', '[]',
     '[".kimi/skills"]',
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('53861a53', NULL, 'OpenCode', NULL, NULL, NULL,
     'opencode', 'acp', 'builtin',
     '{"binary_name":"opencode"}',
     1, 'opencode', '["acp"]', '[]',
     '[".opencode/skills"]',
     '{"supports_side_question":false}',
     'build',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('3cd9d436', NULL, 'Copilot', NULL, NULL, NULL,
     'copilot', 'acp', 'builtin',
     '{"binary_name":"copilot"}',
     1, 'copilot', '["--acp","--stdio"]', '[]',
     NULL,
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('1e4afc51', NULL, 'Qoder', NULL, NULL, NULL,
     'qoder', 'acp', 'builtin',
     '{"binary_name":"qoder"}',
     1, 'qoder', '["--acp"]', '[]',
     NULL,
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('65d0f5b2', NULL, 'Vibe', NULL, NULL, NULL,
     'vibe', 'acp', 'builtin',
     '{"binary_name":"vibe"}',
     1, 'vibe', '[]', '[]',
     '[".vibe/skills"]',
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('a0dfb1ec', NULL, 'Cursor', NULL, NULL, NULL,
     'cursor', 'acp', 'builtin',
     '{"binary_name":"cursor"}',
     1, 'cursor', '["acp"]', '[]',
     '[".cursor/skills"]',
     '{"supports_side_question":false}',
     'agent',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('e044000d', NULL, 'Kiro', NULL, NULL, NULL,
     'kiro', 'acp', 'builtin',
     '{"binary_name":"kiro"}',
     1, 'kiro', '["acp"]', '[]',
     NULL,
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('55f3ed1c', NULL, 'Hermes', NULL, NULL, NULL,
     'hermes', 'acp', 'builtin',
     '{"binary_name":"hermes"}',
     1, 'hermes', '["acp"]', '[]',
     NULL,
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('346b0041', NULL, 'Snow', NULL, NULL, NULL,
     'snow', 'acp', 'builtin',
     '{"binary_name":"snow"}',
     1, 'snow', '["--acp"]', '[]',
     NULL,
     '{"supports_side_question":false}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000);

-- ── Seed: non-ACP builtins + internal ─────────────────────────────────

INSERT OR IGNORE INTO agent_metadata
    (id, icon, name, name_i18n, description, description_i18n,
     backend, agent_type, agent_source, agent_source_info,
     enabled, command, args, env, native_skills_dirs, behavior_policy, yolo_id,
     agent_capabilities, auth_methods, config_options,
     available_modes, available_models, available_commands,
     created_at, updated_at)
VALUES
    ('fb1083a5', NULL, 'Nanobot', NULL, NULL, NULL,
     NULL, 'nanobot', 'builtin',
     '{"binary_name":"nanobot"}',
     1, 'nanobot', '["--experimental-acp"]', '[]',
     NULL,
     '{}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('f9f61666', NULL, 'OpenClaw Gateway', NULL, NULL, NULL,
     NULL, 'openclaw-gateway', 'builtin',
     '{"binary_name":"openclaw"}',
     1, 'openclaw', '[]', '[]',
     NULL,
     '{}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000),

    ('632f31d2', NULL, 'Aion CLI', NULL, NULL, NULL,
     NULL, 'aionrs', 'internal',
     '{}',
     1, NULL, '[]', '[]',
     '[".aionrs/skills"]',
     '{}',
     'yolo',
     NULL, NULL, NULL, NULL, NULL, NULL,
     unixepoch('now','subsec')*1000, unixepoch('now','subsec')*1000);
