-- Credit-based billing system for CRM Swift.
-- Adds consumption tracking alongside existing plan tiers.
-- Plans have a monthly credit allowance; actions deduct from it.

ALTER TABLE plans ADD COLUMN IF NOT EXISTS monthly_credits INTEGER DEFAULT 0;

-- Track credit consumption per tenant per billing period
CREATE TABLE IF NOT EXISTS credit_usage (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    credits_used INTEGER NOT NULL DEFAULT 0,
    credits_remaining INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Individual credit transactions (audit trail per action)
CREATE TABLE IF NOT EXISTS credit_transactions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    action_type VARCHAR(100) NOT NULL,  -- which action consumed credits
    credits INTEGER NOT NULL,           -- positive = buy, negative = consume
    description TEXT,                    -- what happened
    entity_type VARCHAR(50),            -- contact, automation, etc
    entity_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Per-tier credit allowances and per-action costs
-- Each plan tier gets a monthly credit pool + overage rates
INSERT INTO plans (name, slug, description, price_monthly, price_yearly, features, checkout_url, sort_order, monthly_credits) VALUES
('Free', 'free', 'CRM Swift Trial — learn the system', 0, 0,
 '{"max_contacts": 100, "max_users": 1, "pipelines": 1, "ai_enabled": false, "automation_enabled": true, "integrations": 0, "storage_gb": 0.1, "api_calls_per_day": 100, "onboarding_checklists": true, "account_health_monitoring": true}'::jsonb,
 'https://mintbird.com/checkout/crm-swift-free', 0, 200),
('Starter', 'starter', 'Small agencies — get leads managed', 29, 290,
 '{"max_contacts": 500, "max_users": 3, "pipelines": 2, "ai_enabled": false, "automation_enabled": true, "integrations": 1, "storage_gb": 1, "api_calls_per_day": 1000, "onboarding_checklists": true, "account_health_monitoring": true}'::jsonb,
 'https://mintbird.com/checkout/crm-swift-starter', 1, 2000),
('Professional', 'professional', 'Growing agencies — automation + AI', 79, 790,
 '{"max_contacts": 5000, "max_users": 15, "pipelines": 5, "ai_enabled": true, "automation_enabled": true, "integrations": 5, "storage_gb": 10, "api_calls_per_day": 10000, "onboarding_checklists": true, "account_health_monitoring": true, "ai_message_composition": true}'::jsonb,
 'https://mintbird.com/checkout/crm-swift-pro', 2, 10000),
('Enterprise', 'enterprise', 'Large agencies — unlimited everything', 199, 1990,
 '{"max_contacts": 50000, "max_users": 100, "pipelines": 20, "ai_enabled": true, "automation_enabled": true, "integrations": 50, "storage_gb": 100, "api_calls_per_day": 100000, "onboarding_checklists": true, "account_health_monitoring": true, "ai_message_composition": true}'::jsonb,
 'https://mintbird.com/checkout/crm-swift-enterprise', 3, 50000)
ON CONFLICT (slug) DO NOTHING;
