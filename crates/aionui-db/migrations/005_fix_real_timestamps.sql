-- Fix legacy rows where timestamps were stored as REAL (float) instead of INTEGER.
-- sqlx cannot decode REAL values into i64, causing the conversations list API to fail.
UPDATE conversations
SET created_at = CAST(created_at AS INTEGER)
WHERE typeof(created_at) = 'real';

UPDATE conversations
SET updated_at = CAST(updated_at AS INTEGER)
WHERE typeof(updated_at) = 'real';
