-- 027_create_affiliate_products.sql
-- Affiliate product board — products/services affiliates can promote
-- Tied to CRM Swift tags for FunnelSwift sync
-- Also adds a public-facing webhook endpoint for OpenClaw/n8n automation

-- Affiliate products (the product board)
CREATE TABLE IF NOT EXISTS affiliate_products (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    price DECIMAL(10,2) NOT NULL DEFAULT 0,
    commission_rate DECIMAL(5,2) DEFAULT 10.00,
    commission_type VARCHAR(20) NOT NULL DEFAULT 'percentage' CHECK (commission_type IN ('percentage','fixed')),
    commission_amount DECIMAL(10,2) DEFAULT 0,
    tag_id UUID REFERENCES tags(id) ON DELETE SET NULL,
    image_url TEXT,
    checkout_url TEXT,
    is_active BOOLEAN DEFAULT true,
    sort_order INT DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_affiliate_products_tenant ON affiliate_products(tenant_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_products_active ON affiliate_products(tenant_id, is_active);
CREATE INDEX IF NOT EXISTS idx_affiliate_products_tag ON affiliate_products(tag_id);

-- Public automation webhook (for OpenClaw, n8n, CheatLayer to call)
CREATE TABLE IF NOT EXISTS automation_webhooks (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    webhook_token VARCHAR(255) NOT NULL UNIQUE DEFAULT encode(gen_random_bytes(32), 'hex'),
    allowed_actions TEXT[] NOT NULL DEFAULT '{}',
    rate_limit_per_minute INT DEFAULT 60,
    last_used_at TIMESTAMPTZ,
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_automation_webhooks_token ON automation_webhooks(webhook_token);
CREATE INDEX IF NOT EXISTS idx_automation_webhooks_tenant ON automation_webhooks(tenant_id);

-- Webhook action log
CREATE TABLE IF NOT EXISTS automation_webhook_logs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    webhook_id UUID REFERENCES automation_webhooks(id) ON DELETE CASCADE,
    action VARCHAR(100) NOT NULL,
    request_body JSONB,
    response_status INT,
    response_body TEXT,
    ip_address INET,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_webhook_logs_webhook ON automation_webhook_logs(webhook_id);
CREATE INDEX IF NOT EXISTS idx_webhook_logs_created ON automation_webhook_logs(webhook_id, created_at DESC);

-- Auto-generate a webhook for every new tenant
CREATE OR REPLACE FUNCTION auto_create_webhook()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO automation_webhooks (id, tenant_id, name, allowed_actions)
    VALUES (
        gen_random_uuid(),
        NEW.id,
        'Auto-generated for ' || NEW.name,
        ARRAY['contacts.list', 'contacts.create', 'contacts.get', 'contacts.update',
              'tags.list', 'tags.assign', 'tags.unassign',
              'lists.list', 'lists.members',
              'pipelines.list', 'pipelines.opportunities', 'pipelines.stages', 'pipelines.create_stage',
              'affiliates.profile', 'affiliates.referrals', 'affiliates.stats',
              'affiliate_products.list', 'affiliate_products.my', 'affiliate_products.select', 'affiliate_products.unselect',
              'tenants.create', 'tenants.settings',
              'users.invite', 'users.list',
              'webhooks.generate', 'webhooks.revoke', 'webhooks.list',
              'scoring.calculate',
              'analytics.contacts',
              'audit.log',
              'search.query',
              'comms.send', 'comms.templates',
              'automation.list', 'automation.trigger',
              'events.ingest',
              'native.connect', 'native.sync.push', 'native.sync.pull',
              'billing.plans', 'billing.credits',
              'ai.assess', 'ai.compose', 'ai.recommend',
              'directory.listings', 'directory.listings.create', 'directory.listings.get',
              'directory.listings.update', 'directory.reviews', 'directory.followups',
              'directory.analytics', 'directory.health']
    );
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trigger_auto_create_webhook ON tenants;
CREATE TRIGGER trigger_auto_create_webhook
    AFTER INSERT ON tenants
    FOR EACH ROW
    EXECUTE FUNCTION auto_create_webhook();
