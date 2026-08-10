-- 025_create_native_app_connections.sql
-- Native app connector system — CRM Swift talks to your other apps natively
--
-- AdaSwift -> Admin only (client viewing portal)
-- CheatLayer -> Admin only (RPA automation engine)
-- FunnelSwift -> Admin + Tenant (sales funnels)
-- WorkflowSwift -> Admin + Tenant (n8n workflows)
-- MissedCall Responder -> Admin + Tenant (missed call handling / SMS auto-reply)
-- Multi-Directory App -> Admin + Tenant (business directory across multiple platforms)
--
-- Instead of Mailgun/SMTP.com sending welcome emails, CRM Swift triggers
-- Ada campaigns on new client/account creation.

-- 1. Global app registry (seeded by admin on first deploy)
CREATE TABLE IF NOT EXISTS native_apps (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    slug VARCHAR(100) NOT NULL UNIQUE,
    name VARCHAR(255) NOT NULL,
    description TEXT DEFAULT '',
    auth_type VARCHAR(20) NOT NULL CHECK (auth_type IN ('api_key', 'oauth2', 'basic')),
    auth_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    access_level VARCHAR(20) NOT NULL CHECK (access_level IN ('admin', 'admin_tenant')),
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 2. Per-tenant app connections (stored credentials)
CREATE TABLE IF NOT EXISTS app_connections (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    app_slug VARCHAR(100) NOT NULL,
    credentials JSONB NOT NULL DEFAULT '{}'::jsonb,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(20) NOT NULL DEFAULT 'disconnected' CHECK (status IN ('connected', 'disconnected', 'error')),
    last_test_at TIMESTAMPTZ,
    last_test_ok BOOLEAN,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id, app_slug)
);

CREATE INDEX IF NOT EXISTS idx_app_connections_tenant ON app_connections(tenant_id);
CREATE INDEX IF NOT EXISTS idx_app_connections_slug ON app_connections(tenant_id, app_slug);
CREATE INDEX IF NOT EXISTS idx_app_connections_status ON app_connections(tenant_id, status);

-- 3. Sync history log
CREATE TABLE IF NOT EXISTS app_sync_logs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    app_slug VARCHAR(100) NOT NULL,
    direction VARCHAR(10) NOT NULL CHECK (direction IN ('push', 'pull')),
    entity_type VARCHAR(50) NOT NULL,
    records_processed INT DEFAULT 0,
    records_succeeded INT DEFAULT 0,
    records_failed INT DEFAULT 0,
    error_log JSONB,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    status VARCHAR(20) NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'completed', 'failed'))
);

CREATE INDEX IF NOT EXISTS idx_app_sync_logs_tenant ON app_sync_logs(tenant_id);
CREATE INDEX IF NOT EXISTS idx_app_sync_logs_slug ON app_sync_logs(tenant_id, app_slug, started_at DESC);

-- 4. Admin-level global app configs
CREATE TABLE IF NOT EXISTS app_admin_configs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    app_slug VARCHAR(100) NOT NULL UNIQUE,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 5. Ada Campaign Triggers — replaces Mailgun/SMTP.com for welcome emails
-- CRM Swift automation rules can trigger an Ada campaign instead of sending
-- email directly. This means welcome emails, scan reports, and account
-- activation messages go through AdaSwift's campaign engine.
CREATE TABLE IF NOT EXISTS ada_campaign_triggers (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    trigger_on VARCHAR(50) NOT NULL CHECK (trigger_on IN ('user_created', 'contact_created', 'account_activated', 'scan_complete', 'referral_confirmed', 'commission_earned', 'payout_processed', 'affiliate_activated')),
    ada_campaign_id VARCHAR(255) NOT NULL,
    schedule_delay_minutes INT DEFAULT 0,
    active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ada_triggers_tenant ON ada_campaign_triggers(tenant_id);
CREATE INDEX IF NOT EXISTS idx_ada_triggers_trigger ON ada_campaign_triggers(tenant_id, trigger_on, active);

-- Seed the 6 native apps
INSERT INTO native_apps (slug, name, description, auth_type, auth_config, access_level) VALUES
    ('adaswift', 'AdaSwift Console', 'Client viewing portal — clients see reports, proposals, account status. Admin-only.', 'api_key', '{"fields": ["api_key", "base_url"]}'::jsonb, 'admin'),
    ('cheatlayer', 'CheatLayer', 'RPA automation engine — browser automation, scraping, form filling. Admin-only.', 'api_key', '{"fields": ["api_key", "base_url"]}'::jsonb, 'admin'),
    ('funnelswift', 'FunnelSwift', 'Sales funnel builder (Expo React Native). Tenants connect their own account.', 'api_key', '{"fields": ["api_key", "webhook_secret"]}'::jsonb, 'admin_tenant'),
    ('workflowswift', 'WorkflowSwift Automation', 'n8n-based workflow automation with Supabase backend.', 'api_key', '{"fields": ["api_key", "base_url"]}'::jsonb, 'admin_tenant'),
    ('missedcall-responder', 'MissedCall Responder', 'Callback Pro SaaS — missed call handling, SMS auto-reply, hybrid LLM suite, lead kanban.', 'api_key', '{"fields": ["api_key", "base_url"]}'::jsonb, 'admin_tenant'),
    ('multi-directory', 'Multi-Directory App', 'Multi-tenant business directory system with automated follow-up across directories. Admin-only — internal tool for creating directories.', 'api_key', '{"fields": ["api_key", "base_url"]}'::jsonb, 'admin')
ON CONFLICT (slug) DO NOTHING;
