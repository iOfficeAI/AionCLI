-- Enable behavior_policy flags on the Claude catalog row.
--
-- 006_agent_metadata.sql originally seeded Claude with
-- {"supports_side_question":true}. Two new capability bits were added
-- (self_identity_sticky, session_load_via_meta_field) and Claude needs
-- both of them. We cannot edit 006 because sqlx migration checksums
-- would mismatch for already-upgraded users, so patch the existing row
-- here. SQLite's json_set merges without clobbering unrelated fields,
-- making this safe to re-run when other flags get added later.

UPDATE agent_metadata
SET behavior_policy = json_set(
        COALESCE(behavior_policy, '{}'),
        '$.self_identity_sticky', json('true'),
        '$.session_load_via_meta_field', json('true')
    )
WHERE backend = 'claude';
