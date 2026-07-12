-- Phase 1: Super Admin Plans & Tenant Plan Assignments
-- Creates the plans table with feature flags and assigns plan_id to tenants

CREATE TABLE IF NOT EXISTS plans (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(255) NOT NULL,
    description TEXT,
    price_monthly DECIMAL(10,2) DEFAULT 0,
    price_yearly DECIMAL(10,2) DEFAULT 0,
    max_contacts INTEGER DEFAULT -1, -- -1 = unlimited
    max_deals INTEGER DEFAULT -1,
    max_users INTEGER DEFAULT -1,
    max_storage_mb INTEGER DEFAULT 100,
    features JSONB DEFAULT '{}'::jsonb, -- {"api_access":true,"ai":false,"campaigns":false,"white_label":false,"portfolio":false}
    payment_link TEXT, -- Mintbird/Stripe URL
    is_active BOOLEAN DEFAULT true,
    sort_order INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE tenants ADD COLUMN IF NOT EXISTS plan_id UUID REFERENCES plans(id);
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS is_portfolio BOOLEAN DEFAULT false;
