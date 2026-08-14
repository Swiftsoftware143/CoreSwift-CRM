-- 034_fix_contact_scores.sql
-- Align scores table columns with Rust model.
-- Idempotent: works whether the table is named 'contact_scores' or 'scores'.

DO $$
DECLARE
    target_table TEXT;
BEGIN
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='contact_scores') THEN
        target_table := 'contact_scores';
    ELSIF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='scores') THEN
        target_table := 'scores';
    ELSE
        RAISE NOTICE 'Neither contact_scores nor scores table exists; skipping';
        RETURN;
    END IF;

    -- Rename calculated_at -> created_at if needed
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = target_table AND column_name = 'calculated_at'
    ) AND NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = target_table AND column_name = 'created_at'
    ) THEN
        EXECUTE format('ALTER TABLE %I RENAME COLUMN calculated_at TO created_at', target_table);
    END IF;

    -- Add missing columns idempotently
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = target_table AND column_name = 'last_event_type') THEN
        EXECUTE format('ALTER TABLE %I ADD COLUMN last_event_type VARCHAR(100)', target_table);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = target_table AND column_name = 'last_event_at') THEN
        EXECUTE format('ALTER TABLE %I ADD COLUMN last_event_at TIMESTAMPTZ', target_table);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = target_table AND column_name = 'updated_at') THEN
        EXECUTE format('ALTER TABLE %I ADD COLUMN updated_at TIMESTAMPTZ', target_table);
    END IF;
END $$;
