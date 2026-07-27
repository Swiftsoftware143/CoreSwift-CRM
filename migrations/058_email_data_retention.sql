-- Migration 058: Email data retention — per-tenant auto-purge settings
-- Adds retention_days and last_purged_at to tenant_email_limits.
-- The purge background task uses these to delete old outbound_messages
-- and related email events.

-- Extend tenant_email_limits with retention columns.
-- Use DO block so it's idempotent whether the table/column already exists.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'tenant_email_limits') THEN
        -- Add retention_days column if it doesn't exist
        IF NOT EXISTS (
            SELECT 1 FROM information_schema.columns
            WHERE table_name = 'tenant_email_limits' AND column_name = 'retention_days'
        ) THEN
            ALTER TABLE tenant_email_limits
              ADD COLUMN retention_days INTEGER NOT NULL DEFAULT 365;
        END IF;

        -- Add last_purged_at column if it doesn't exist
        IF NOT EXISTS (
            SELECT 1 FROM information_schema.columns
            WHERE table_name = 'tenant_email_limits' AND column_name = 'last_purged_at'
        ) THEN
            ALTER TABLE tenant_email_limits
              ADD COLUMN last_purged_at TIMESTAMPTZ;
        END IF;
    END IF;
END $$;

-- Add index on outbound_messages created_at to speed up purge queries
CREATE INDEX IF NOT EXISTS idx_outbound_messages_created
  ON outbound_messages(tenant_id, created_at);

-- Add index on events created_at for email-related event purge
CREATE INDEX IF NOT EXISTS idx_events_email_created
  ON events(tenant_id, created_at)
  WHERE source = 'private_email';
