-- 049_google_calendar.sql
-- Google Calendar Integration for booking_calendars

ALTER TABLE booking_calendars ADD COLUMN IF NOT EXISTS google_refresh_token TEXT;
ALTER TABLE booking_calendars ADD COLUMN IF NOT EXISTS google_calendar_id TEXT;

-- Index for efficient lookup by google calendar id
CREATE INDEX IF NOT EXISTS idx_booking_calendars_google ON booking_calendars(tenant_id, google_calendar_id) WHERE google_calendar_id IS NOT NULL;
