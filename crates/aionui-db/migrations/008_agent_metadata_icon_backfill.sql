-- Backfill builtin/internal logo asset paths served by aionui-backend.
--
-- The frontend now treats `agent_metadata.icon` as the primary source of
-- truth for agent logos. Extension and remote agents keep their existing
-- icon channels and are intentionally untouched here.

UPDATE agent_metadata SET icon = '/api/assets/logos/ai-major/claude.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '2d23ff1c';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/coding/codex.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '8e1acf31';
UPDATE agent_metadata SET icon = '/api/assets/logos/ai-major/gemini.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = 'cc126dd5';
UPDATE agent_metadata SET icon = '/api/assets/logos/ai-china/qwen.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '26a946ed';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/coding/codebuddy.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '8b20fd41';
UPDATE agent_metadata SET icon = '/api/assets/logos/brand/droid.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = 'da386544';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/goose.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '600c6601';
UPDATE agent_metadata SET icon = '/api/assets/logos/brand/auggie.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = 'eb895030';
UPDATE agent_metadata SET icon = '/api/assets/logos/ai-china/kimi.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = 'e241c49c';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/coding/opencode-light.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '53861a53';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/github.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '3cd9d436';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/coding/qoder.png', updated_at = unixepoch('now','subsec')*1000 WHERE id = '1e4afc51';
UPDATE agent_metadata SET icon = '/api/assets/logos/ai-major/mistral.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '65d0f5b2';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/coding/cursor.png', updated_at = unixepoch('now','subsec')*1000 WHERE id = 'a0dfb1ec';
UPDATE agent_metadata SET icon = '/api/assets/logos/brand/hermes.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '55f3ed1c';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/coding/snow.png', updated_at = unixepoch('now','subsec')*1000 WHERE id = '346b0041';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/nanobot.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = 'fb1083a5';
UPDATE agent_metadata SET icon = '/api/assets/logos/tools/openclaw.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = 'f9f61666';
UPDATE agent_metadata SET icon = '/api/assets/logos/brand/aion.svg', updated_at = unixepoch('now','subsec')*1000 WHERE id = '632f31d2';
