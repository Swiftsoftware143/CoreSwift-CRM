-- 012_create_lists.sql
-- Static and dynamic contact lists

CREATE TABLE IF NOT EXISTS lists (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    list_type VARCHAR(20) NOT NULL DEFAULT 'static' CHECK (list_type IN ('static', 'dynamic')),
    dynamic_rules JSONB DEFAULT '[]'::jsonb,
    member_count INTEGER DEFAULT 0,
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_lists_tenant_id ON lists(tenant_id);
CREATE INDEX IF NOT EXISTS idx_lists_type ON lists(tenant_id, list_type);
