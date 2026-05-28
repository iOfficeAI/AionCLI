-- Migration 007: add logical-delete support for MCP servers and rename the
-- ambiguous `status` column to `last_test_status`.
ALTER TABLE mcp_servers ADD COLUMN deleted_at INTEGER;
ALTER TABLE mcp_servers RENAME COLUMN status TO last_test_status;
CREATE INDEX IF NOT EXISTS idx_mcp_servers_deleted_at ON mcp_servers(deleted_at);
