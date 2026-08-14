-- 041_fix_pipeline_stages_position.sql
-- Fix C4: Rename sort_order -> position to match code expectations
-- Idempotent: only renames if sort_order exists and position does not.

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'pipeline_stages' AND column_name = 'sort_order'
    ) AND NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'pipeline_stages' AND column_name = 'position'
    ) THEN
        ALTER TABLE pipeline_stages RENAME COLUMN sort_order TO position;
    END IF;
END $$;
