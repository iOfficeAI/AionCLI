-- 013_agent_metadata_drop_name_unique.sql
-- Drop UNIQUE(agent_source, name) to allow multiple custom agents with the
-- same display name (PRD F-CAGENT-12: no duplicate-name validation).
-- Uniqueness is enforced solely by PRIMARY KEY(id).
--
-- SQLite cannot alter constraints in place: rebuild table, copy rows, swap.

PRAGMA foreign_keys = OFF;

CREATE TABLE agent_metadata_new (
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

    sort_order          INTEGER NOT NULL DEFAULT 1000,

    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
    -- No UNIQUE(agent_source, name)
);

INSERT INTO agent_metadata_new
SELECT id, icon, name, name_i18n, description, description_i18n,
       backend, agent_type, agent_source, agent_source_info,
       enabled, command, args, env, native_skills_dirs,
       behavior_policy, yolo_id,
       agent_capabilities, auth_methods, config_options,
       available_modes, available_models, available_commands,
       sort_order,
       created_at, updated_at
FROM agent_metadata;

DROP TABLE agent_metadata;
ALTER TABLE agent_metadata_new RENAME TO agent_metadata;

CREATE INDEX IF NOT EXISTS idx_agent_metadata_backend    ON agent_metadata(backend);
CREATE INDEX IF NOT EXISTS idx_agent_metadata_agent_type ON agent_metadata(agent_type);
CREATE INDEX IF NOT EXISTS idx_agent_metadata_sort_order ON agent_metadata(sort_order);

PRAGMA foreign_keys = ON;
