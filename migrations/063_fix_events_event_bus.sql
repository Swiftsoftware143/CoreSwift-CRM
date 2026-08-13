-- 063_fix_events_event_bus.sql
--
-- The live `events` table was created by a calendar feature with columns
-- (title, event_date, start_time, end_time, location, ...) but the active
-- event-bus code (events/handlers.rs, dispatcher.rs, private_email, telnyx,
-- webhook/actions) depends on migration 021's event-bus schema:
--   source, event_type, entity_type, entity_id, payload, raw_headers,
--   processed, processed_at
--
-- The daily purge job failed on ALL tenants with:
--   "column \"source\" does not exist"
-- because events.source was missing (events was the calendar variant).
--
-- The table is empty (0 rows) and no live code references the calendar
-- columns, so this is purely additive and safe.

ALTER TABLE events
    ADD COLUMN IF NOT EXISTS source VARCHAR(100),
    ADD COLUMN IF NOT EXISTS entity_type VARCHAR(50),
    ADD COLUMN IF NOT EXISTS entity_id UUID,
    ADD COLUMN IF NOT EXISTS payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS raw_headers JSONB,
    ADD COLUMN IF NOT EXISTS processed BOOLEAN DEFAULT false,
    ADD COLUMN IF NOT EXISTS processed_at TIMESTAMPTZ;

-- Backfill source for any existing rows (none at time of write, but safe)
UPDATE events SET source = 'legacy' WHERE source IS NULL;

-- Indexes matching migration 021
CREATE INDEX IF NOT EXISTS idx_events_tenant ON events(tenant_id);
CREATE INDEX IF NOT EXISTS idx_events_source ON events(tenant_id, source);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(tenant_id, event_type);
CREATE INDEX IF NOT EXISTS idx_events_entity ON events(tenant_id, entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_events_created ON events(tenant_id, created_at DESC);
