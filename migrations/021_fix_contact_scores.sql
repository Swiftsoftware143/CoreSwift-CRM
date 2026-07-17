-- 021_fix_contact_scores.sql
-- Align contact_scores columns with Rust model

-- Rename calculated_at to created_at for Rust model compatibility
ALTER TABLE contact_scores RENAME COLUMN calculated_at TO created_at;

-- Add missing columns if they don't already exist (idempotent)
ALTER TABLE contact_scores ADD COLUMN IF NOT EXISTS last_event_type VARCHAR(100);
ALTER TABLE contact_scores ADD COLUMN IF NOT EXISTS last_event_at TIMESTAMPTZ;
ALTER TABLE contact_scores ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ;
