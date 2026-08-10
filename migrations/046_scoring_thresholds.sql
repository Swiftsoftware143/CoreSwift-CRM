-- 046_scoring_thresholds.sql
-- Scoring threshold rules: map score ranges to pipeline stage transitions

CREATE TABLE IF NOT EXISTS scoring_thresholds (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    pipeline_id UUID NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
    min_score INT NOT NULL,
    max_score INT,
    target_stage_id UUID NOT NULL REFERENCES pipeline_stages(id) ON DELETE CASCADE,
    action TEXT NOT NULL DEFAULT 'move_stage',
    action_config JSONB DEFAULT '{}'::jsonb,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_scoring_thresholds_tenant ON scoring_thresholds(tenant_id);
CREATE INDEX IF NOT EXISTS idx_scoring_thresholds_score ON scoring_thresholds(tenant_id, min_score);
