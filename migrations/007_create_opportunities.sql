-- 007_create_opportunities.sql
-- Deals/opportunities linked to contacts and pipeline stages

CREATE TABLE IF NOT EXISTS opportunities (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    pipeline_id UUID NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
    stage_id UUID NOT NULL REFERENCES pipeline_stages(id) ON DELETE RESTRICT,
    contact_id UUID REFERENCES contacts(id) ON DELETE SET NULL,
    company_id UUID REFERENCES companies(id) ON DELETE SET NULL,
    name VARCHAR(255) NOT NULL,
    value DECIMAL(15, 2) DEFAULT 0,
    currency VARCHAR(3) DEFAULT 'USD',
    probability INTEGER DEFAULT 0,
    expected_close_date DATE,
    source VARCHAR(100),
    notes TEXT,
    metadata JSONB DEFAULT '{}'::jsonb,
    is_won BOOLEAN DEFAULT false,
    is_lost BOOLEAN DEFAULT false,
    lost_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_opportunities_tenant_id ON opportunities(tenant_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_pipeline ON opportunities(pipeline_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_stage ON opportunities(stage_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_contact ON opportunities(contact_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_value ON opportunities(tenant_id, value);

-- Stage history for tracking movement over time
CREATE TABLE IF NOT EXISTS opportunity_stage_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    opportunity_id UUID NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    from_stage_id UUID REFERENCES pipeline_stages(id) ON DELETE SET NULL,
    to_stage_id UUID NOT NULL REFERENCES pipeline_stages(id) ON DELETE SET NULL,
    moved_by UUID REFERENCES users(id) ON DELETE SET NULL,
    moved_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_stage_history_opportunity ON opportunity_stage_history(opportunity_id);
CREATE INDEX IF NOT EXISTS idx_stage_history_time ON opportunity_stage_history(moved_at);
