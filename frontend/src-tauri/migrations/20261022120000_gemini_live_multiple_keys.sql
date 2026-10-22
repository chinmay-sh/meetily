-- Migration: multiple Gemini Live API keys (fork addition)
-- Ordered JSON array; the first key is the primary, the rest are backups.
-- The single api_key column from the previous migration is migrated into this list on load.

ALTER TABLE gemini_live_settings ADD COLUMN api_keys TEXT NOT NULL DEFAULT '[]';
