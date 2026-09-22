-- 037_fresh_install_base_tables.sql  —  kanban t_c76d941e
--
-- WHY THIS FILE EXISTS (measured on the live host, 2026-09-22)
--   The repo's migration set could not build its own database. On an EMPTY database the
--   chain dies at 050:
--       error: while executing migration 50: error returned from database:
--              relation "notification_rules" does not exist
--   ...and 6 files in total cannot run on a fresh install, because each one reconciles an
--   object that the live database already had but that NO migration in this directory
--   creates (it was created out-of-band, long before the migration set existed):
--       050 notification_rules   (ALTERs a table nothing creates)
--       058 outbound_messages    (INDEXes a table first declared by 067, i.e. 9 files later)
--       062 business_profiles.name (backfills business_name FROM name — no migration adds name)
--       063 events               (ALTERs a table first declared by 067)
--       067 notifications        (UPDATE ... COALESCE(body, title) — body/title are live-only columns)
--       083 tickets.source       (adds the tickets_source_check that carries 'portal'; 060's
--                                 tickets has no source column)
--   Consequence before this file: none of 050..085 could be reproduced from the repo, so a
--   fresh/restored database silently came up without a single one of them.
--
-- WHY VERSION 037
--   It has to run BEFORE 050, the first dependent file. `migrations/` has exactly two free
--   version slots — 037 and 082 — because earlier renames (see commit 6fbbeb4) left them
--   vacant, and appending a higher number cannot help a file that runs earlier. 082 is used
--   by `082_tickets_source.sql` for the same reason (tickets is created by 060).
--   Adding a pending migration whose version is BELOW the applied maximum is safe here:
--   sqlx 0.8's Migrator::run applies every migration that is not in the applied set,
--   independent of order (sqlx-core-0.8.6/src/migrate/migrator.rs:168-182), and
--   validate_applied_migrations only rejects an APPLIED version that has no file.
--
-- WHY THE SHAPES BELOW ARE THE LIVE ONES
--   Every later file, and the application code, was written against the live shape. So a
--   fresh install converges on the same shape the live database has, instead of inventing a
--   third one. Column order, types, nullability and defaults are copied from
--   information_schema on coreswift_crm (2026-09-22).
--
-- ON A DATABASE THAT ALREADY HAS THESE OBJECTS (live) EVERY STATEMENT IS A NO-OP:
--   CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS / ADD COLUMN IF NOT EXISTS.
--   Proven by a column+constraint shape diff of live before and after this file was applied.

-- ── message_templates (050 re-guards its channel CHECK; 067 declares it, 17 files later) ────
-- The channel CHECK is deliberately NOT declared here: 050 owns it (it widens the list to
-- include 'whatsapp' behind a guard), so declaring it here too would only duplicate that step.
CREATE TABLE IF NOT EXISTS message_templates (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  UUID NOT NULL REFERENCES tenants(id),
    name       VARCHAR(255) NOT NULL,
    channel    VARCHAR(255) DEFAULT 'email',
    subject    VARCHAR(255),
    body       TEXT,
    variables  JSONB DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_message_templates_tenant ON message_templates(tenant_id);

-- ── notification_rules (050 reconciles and then indexes it) ────────────────────────────────
CREATE TABLE IF NOT EXISTS notification_rules (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id     UUID NOT NULL REFERENCES tenants(id),
    event_type    VARCHAR(255) NOT NULL,
    channel       VARCHAR(255) DEFAULT 'in_app',
    template_id   UUID,
    is_active     BOOLEAN DEFAULT true,
    created_at    TIMESTAMPTZ DEFAULT now(),
    trigger_event TEXT,
    action        TEXT,
    target_entity TEXT,
    config        JSONB DEFAULT '{}'::jsonb,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_notification_rules_tenant ON notification_rules(tenant_id);
CREATE INDEX IF NOT EXISTS idx_notification_rules_trigger ON notification_rules(tenant_id, trigger_event) WHERE is_active = true;

-- ── outbound_messages (058 indexes it; 067 declares it, 9 files later) ─────────────────────
CREATE TABLE IF NOT EXISTS outbound_messages (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id     UUID,
    contact_id    UUID,
    channel       TEXT,
    recipient     TEXT,
    subject       TEXT,
    body          TEXT,
    status        TEXT DEFAULT 'pending',
    scheduled_at  TIMESTAMPTZ,
    sent_at       TIMESTAMPTZ,
    created_at    TIMESTAMPTZ DEFAULT now(),
    to_address    TEXT,
    cancelled     BOOLEAN DEFAULT false,
    cc_address    TEXT,
    bcc_address   TEXT,
    message_id    TEXT,
    reply_to      TEXT,
    template_id   UUID,
    template_data JSONB,
    opened_at     TIMESTAMPTZ,
    clicked_at    TIMESTAMPTZ,
    error_message TEXT,
    retry_count   INTEGER DEFAULT 0,
    max_retries   INTEGER DEFAULT 3,
    provider      TEXT
);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_tenant ON outbound_messages(tenant_id);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_status ON outbound_messages(tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_channel ON outbound_messages(tenant_id, channel);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_created ON outbound_messages(tenant_id, created_at);

-- ── events (063 ALTERs it; 067 declares it, 4 files later) ─────────────────────────────────
CREATE TABLE IF NOT EXISTS events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    title       VARCHAR(255) NOT NULL,
    description TEXT,
    event_date  DATE,
    start_time  TIME,
    end_time    TIME,
    location    VARCHAR(255),
    event_type  VARCHAR(255) DEFAULT 'meeting',
    status      VARCHAR(255) DEFAULT 'scheduled',
    contact_id  UUID REFERENCES contacts(id),
    company_id  UUID REFERENCES companies(id),
    deal_id     UUID REFERENCES opportunities(id),
    created_by  UUID REFERENCES users(id),
    created_at  TIMESTAMPTZ DEFAULT now(),
    updated_at  TIMESTAMPTZ DEFAULT now(),
    source      VARCHAR(255),
    entity_type VARCHAR(255),
    entity_id   UUID,
    payload     JSONB NOT NULL DEFAULT '{}'::jsonb,
    raw_headers JSONB,
    processed   BOOLEAN DEFAULT false,
    processed_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_events_tenant ON events(tenant_id);
CREATE INDEX IF NOT EXISTS idx_events_date ON events(event_date);
CREATE INDEX IF NOT EXISTS idx_events_source ON events(tenant_id, source);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(tenant_id, event_type);
CREATE INDEX IF NOT EXISTS idx_events_entity ON events(tenant_id, entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_events_created ON events(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_events_email_created ON events(tenant_id, created_at) WHERE source::text = 'private_email';

-- ── notifications (067 backfills message FROM body/title and read FROM is_read) ────────────
CREATE TABLE IF NOT EXISTS notifications (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id         UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id           UUID REFERENCES users(id) ON DELETE CASCADE,
    title             VARCHAR(255) NOT NULL,
    body              TEXT,
    notification_type VARCHAR(255) DEFAULT 'info',
    is_read           BOOLEAN DEFAULT false,
    metadata          JSONB DEFAULT '{}'::jsonb,
    created_at        TIMESTAMPTZ DEFAULT now(),
    message           TEXT,
    read              BOOLEAN DEFAULT false
);
CREATE INDEX IF NOT EXISTS idx_notifications_tenant ON notifications(tenant_id);
CREATE INDEX IF NOT EXISTS idx_notifications_user ON notifications(tenant_id, user_id);
CREATE INDEX IF NOT EXISTS idx_notifications_user_read ON notifications(tenant_id, user_id, read);

-- ── business_profiles.name (062's backfill reads it; 023 does not create it) ───────────────
ALTER TABLE business_profiles ADD COLUMN IF NOT EXISTS name TEXT;
