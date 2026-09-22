-- 076_google_calendar_push_channels.sql
-- Registry of the Google Calendar push channels this deployment registered via
-- `POST /calendars/{id}/events/watch`.
--
-- POST /api/google-calendar/webhook is a PUBLIC machine surface: Google sends no Authorization
-- header, so nothing about the request is self-authenticating. Until this table existed the handler
-- verified nothing at all and answered 200 to every caller in the world (reproduced live
-- 2026-09-22, card t_5dab8cff). A notification is now accepted only when:
--   * X-Goog-Channel-ID names a row here that is active and unexpired,
--   * X-Goog-Resource-ID equals the `resourceId` Google returned at registration, and
--   * X-Goog-Channel-Token hashes to `token_hash`.
-- The tenant is then read FROM THE ROW, never from the caller.
--
-- token_hash holds sha256-hex of the per-channel token WE generated and handed to Google at
-- registration (Google echoes it verbatim on every notification). Only the digest is stored, so a
-- dump of this table cannot be replayed to forge a notification — the `personal_api_keys`
-- convention for a machine-generated high-entropy secret.

CREATE TABLE IF NOT EXISTS google_calendar_push_channels (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id            UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    booking_calendar_id  UUID NOT NULL REFERENCES booking_calendars(id) ON DELETE CASCADE,
    channel_id           TEXT NOT NULL UNIQUE,
    resource_id          TEXT NOT NULL,
    token_hash           TEXT NOT NULL,
    expires_at           TIMESTAMPTZ NOT NULL,
    is_active            BOOLEAN NOT NULL DEFAULT true,
    last_notification_at  TIMESTAMPTZ,
    last_resource_state   TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_gcal_push_channels_calendar
    ON google_calendar_push_channels (booking_calendar_id);

CREATE INDEX IF NOT EXISTS idx_gcal_push_channels_tenant
    ON google_calendar_push_channels (tenant_id);
