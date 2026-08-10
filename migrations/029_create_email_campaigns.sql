-- Email Campaigns Module
-- Groups message templates into sequenced campaigns with scheduled delays

CREATE TABLE IF NOT EXISTS email_campaigns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'paused', 'completed', 'archived')),
    created_by UUID REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_email_campaigns_tenant ON email_campaigns(tenant_id);
CREATE INDEX IF NOT EXISTS idx_email_campaigns_status ON email_campaigns(tenant_id, status);

CREATE TABLE IF NOT EXISTS email_campaign_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    campaign_id UUID NOT NULL REFERENCES email_campaigns(id) ON DELETE CASCADE,
    step_order INTEGER NOT NULL,
    template_name TEXT NOT NULL,
    subject TEXT,
    body TEXT NOT NULL,
    delay_days INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_email_campaign_steps_campaign ON email_campaign_steps(campaign_id);

-- Links campaigns to tags: when a contact gets this tag, start the campaign
CREATE TABLE IF NOT EXISTS email_campaign_triggers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    campaign_id UUID NOT NULL REFERENCES email_campaigns(id) ON DELETE CASCADE,
    tag_id UUID NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    trigger_type TEXT NOT NULL DEFAULT 'tag_assigned'
        CHECK (trigger_type IN ('tag_assigned', 'contact_created', 'manual')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(campaign_id, tag_id)
);

-- Tracks which contacts are in which campaign and what step they're on
CREATE TABLE IF NOT EXISTS email_campaign_enrollments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    campaign_id UUID NOT NULL REFERENCES email_campaigns(id) ON DELETE CASCADE,
    entity_type TEXT NOT NULL DEFAULT 'contact',
    entity_id UUID NOT NULL,
    current_step INTEGER NOT NULL DEFAULT 0,
    total_steps INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'paused', 'completed', 'unsubscribed')),
    next_send_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_email_campaign_enrollments_campaign ON email_campaign_enrollments(campaign_id);
CREATE INDEX IF NOT EXISTS idx_email_campaign_enrollments_entity ON email_campaign_enrollments(entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_email_campaign_enrollments_next_send ON email_campaign_enrollments(next_send_at)
    WHERE status = 'active' AND next_send_at IS NOT NULL;

-- Tag sync log: tracks which tags have been synced between systems
CREATE TABLE IF NOT EXISTS tag_sync_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    target TEXT NOT NULL,
    tag_name TEXT NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('create', 'update', 'delete')),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'synced', 'failed')),
    error_message TEXT,
    synced_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Auto-create campaigns table
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS integration_config JSONB DEFAULT '{}';

-- Row-level security via tenant_id is enforced by the application layer
-- (CRM Swift uses middleware for tenant scoping)
