-- Industry Dashboards for CoreSwift CRM
-- Links each user/tenant to industry dashboard configurations
-- Industry slugs align with template_categories in workflowswift DB

-- Add max_industries to plans table (default 1, -1 = unlimited)
ALTER TABLE plans ADD COLUMN IF NOT EXISTS max_industries INTEGER DEFAULT 1;

-- User industry dashboards: tracks which industries a user has activated
CREATE TABLE IF NOT EXISTS user_industry_dashboards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    industry_slug VARCHAR(100) NOT NULL,
    dashboard_name VARCHAR(255) NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(user_id, industry_slug)
);

CREATE INDEX IF NOT EXISTS idx_user_industry_dashboards_user ON user_industry_dashboards(user_id);
CREATE INDEX IF NOT EXISTS idx_user_industry_dashboards_tenant ON user_industry_dashboards(tenant_id);
CREATE INDEX IF NOT EXISTS idx_user_industry_dashboards_industry ON user_industry_dashboards(industry_slug);

-- Tenant default industry (for new users on this tenant)
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS industry_slug VARCHAR(100) DEFAULT 'site-flipping';

-- Update existing plans with industry limits by matching on name
-- Free/Free Plan: 1 industry, Starter: 2, Pro/Professional: 5, Enterprise: unlimited (-1)
UPDATE plans SET max_industries = 1 WHERE LOWER(name) LIKE '%free%' AND max_industries IS NULL;
UPDATE plans SET max_industries = 2 WHERE LOWER(name) = 'starter' AND max_industries IS NULL;
UPDATE plans SET max_industries = 5 WHERE LOWER(name) IN ('professional', 'pro') AND max_industries IS NULL;
UPDATE plans SET max_industries = -1 WHERE LOWER(name) = 'enterprise' AND max_industries IS NULL;
-- Catch-all for any plans not matched above
UPDATE plans SET max_industries = 1 WHERE max_industries IS NULL;
