-- 020_create_plans_and_affiliates.sql
-- Billing plan tiers, tenant subscriptions, affiliates, referrals, and commission payouts

-- Plan tiers table
CREATE TABLE IF NOT EXISTS plans (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(100) NOT NULL,
    slug VARCHAR(50) NOT NULL UNIQUE,
    description TEXT,
    price_monthly DECIMAL(10,2) NOT NULL DEFAULT 0,
    price_yearly DECIMAL(10,2) NOT NULL DEFAULT 0,
    features JSONB NOT NULL DEFAULT '{}'::jsonb,  -- {"max_contacts": 1000, "ai_enabled": true, "automation_enabled": true, "integrations": 5, "pipelines": 3, "users": 10}
    checkout_url TEXT,  -- Link to Mint Bird / Stripe checkout page
    is_active BOOLEAN DEFAULT true,
    sort_order INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Tenant plan subscriptions
CREATE TABLE IF NOT EXISTS tenant_plans (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    plan_id UUID NOT NULL REFERENCES plans(id) ON DELETE RESTRICT,
    status VARCHAR(20) NOT NULL DEFAULT 'active' CHECK (status IN ('active','trialing','past_due','canceled','expired')),
    billing_cycle VARCHAR(10) NOT NULL DEFAULT 'monthly' CHECK (billing_cycle IN ('monthly','yearly')),
    trial_ends_at TIMESTAMPTZ,
    current_period_starts_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    current_period_ends_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    feature_overrides JSONB DEFAULT '{}'::jsonb,  -- per-tenant overrides
    canceled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id)
);

CREATE INDEX IF NOT EXISTS idx_tenant_plans_tenant ON tenant_plans(tenant_id);
CREATE INDEX IF NOT EXISTS idx_tenant_plans_plan ON tenant_plans(plan_id);

-- Affiliates
CREATE TABLE IF NOT EXISTS affiliates (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code VARCHAR(50) NOT NULL UNIQUE,
    commission_rate DECIMAL(5,2) NOT NULL DEFAULT 10.00,
    commission_type VARCHAR(20) NOT NULL DEFAULT 'percentage' CHECK (commission_type IN ('percentage','fixed')),
    total_earned DECIMAL(12,2) NOT NULL DEFAULT 0,
    total_paid DECIMAL(12,2) NOT NULL DEFAULT 0,
    referral_count INTEGER NOT NULL DEFAULT 0,
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_affiliates_tenant ON affiliates(tenant_id);
CREATE INDEX IF NOT EXISTS idx_affiliates_code ON affiliates(code);
CREATE UNIQUE INDEX IF NOT EXISTS idx_affiliates_user_tenant ON affiliates(tenant_id, user_id);

-- Referrals
CREATE TABLE IF NOT EXISTS referrals (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    affiliate_id UUID NOT NULL REFERENCES affiliates(id) ON DELETE CASCADE,
    referred_tenant_id UUID REFERENCES tenants(id) ON DELETE SET NULL,
    referred_email VARCHAR(255),
    status VARCHAR(20) NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','converted','commissioned','paid','expired')),
    commission_amount DECIMAL(12,2) DEFAULT 0,
    paid_at TIMESTAMPTZ,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_referrals_affiliate ON referrals(affiliate_id);
CREATE INDEX IF NOT EXISTS idx_referrals_status ON referrals(status);

-- Commission payouts
CREATE TABLE IF NOT EXISTS commission_payouts (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    affiliate_id UUID NOT NULL REFERENCES affiliates(id) ON DELETE CASCADE,
    amount DECIMAL(12,2) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','processing','paid','failed')),
    payment_method VARCHAR(50),
    paid_at TIMESTAMPTZ,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_commission_payouts_affiliate ON commission_payouts(affiliate_id);

-- Default plan tiers (Free + paid tiers)
INSERT INTO plans (id, name, slug, description, price_monthly, price_yearly, features, checkout_url, sort_order)
VALUES
(
    uuid_generate_v4(), 'Free', 'free',
    'For testing and small-scale lead management — CRM Swift Trial',
    0.00, 0.00,
    '{"max_contacts": 100, "max_users": 1, "pipelines": 1, "ai_enabled": false, "automation_enabled": true, "integrations": 0, "storage_gb": 0.1, "api_calls_per_day": 100, "onboarding_checklists": true, "account_health_monitoring": true}'::jsonb,
    'https://mintbird.com/checkout/crm-swift-free',
    0
),
(
    uuid_generate_v4(), 'Starter', 'starter',
    'For small agencies getting started with lead management',
    29.00, 290.00,
    '{"max_contacts": 500, "max_users": 3, "pipelines": 2, "ai_enabled": false, "automation_enabled": true, "integrations": 1, "storage_gb": 1, "api_calls_per_day": 1000, "onboarding_checklists": true, "account_health_monitoring": true}'::jsonb,
    'https://mintbird.com/checkout/crm-swift-starter',
    1
),
(
    uuid_generate_v4(), 'Professional', 'professional',
    'For growing agencies that need automation and AI',
    79.00, 790.00,
    '{"max_contacts": 5000, "max_users": 15, "pipelines": 5, "ai_enabled": true, "automation_enabled": true, "integrations": 5, "storage_gb": 10, "api_calls_per_day": 10000, "onboarding_checklists": true, "account_health_monitoring": true}'::jsonb,
    'https://mintbird.com/checkout/crm-swift-pro',
    2
),
(
    uuid_generate_v4(), 'Enterprise', 'enterprise',
    'For large agencies with unlimited everything',
    199.00, 1990.00,
    '{"max_contacts": 50000, "max_users": 100, "pipelines": 20, "ai_enabled": true, "automation_enabled": true, "integrations": 50, "storage_gb": 100, "api_calls_per_day": 100000, "onboarding_checklists": true, "account_health_monitoring": true}'::jsonb,
    'https://mintbird.com/checkout/crm-swift-enterprise',
    3
)
ON CONFLICT (slug) DO NOTHING;

-- CRM Swift trial tag for FunnelSwift integration
INSERT INTO tags (id, tenant_id, category_id, name, color, description)
SELECT
    uuid_generate_v4(), t.id,
    (SELECT c.id FROM tag_categories c WHERE c.tenant_id = t.id AND c.name = 'Status'),
    'CRM Swift - Trial', '#6B7280',
    'Trial tag for FunnelSwift — identifies tenants on free/trial plans that may upgrade'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags x WHERE x.tenant_id = t.id AND x.name = 'CRM Swift - Trial');
