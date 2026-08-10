-- 010_create_score_rules.sql
-- Configurable lead scoring rules

CREATE TABLE IF NOT EXISTS score_rules (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    event_type VARCHAR(100) NOT NULL,
    description TEXT,
    points INTEGER NOT NULL DEFAULT 0,
    direction VARCHAR(10) NOT NULL DEFAULT 'add' CHECK (direction IN ('add', 'subtract')),
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_score_rules_tenant ON score_rules(tenant_id);
CREATE INDEX IF NOT EXISTS idx_score_rules_event ON score_rules(tenant_id, event_type);
