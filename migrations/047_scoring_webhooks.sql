-- 047_scoring_webhooks.sql
-- Webhook targets for score threshold notifications

CREATE TABLE IF NOT EXISTS scoring_webhooks (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    url TEXT NOT NULL,
    min_score INT NOT NULL DEFAULT 0,
    max_score INT,
    event_type VARCHAR(100),
    headers JSONB DEFAULT '{}'::jsonb,
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_fired_at TIMESTAMPTZ,
    failure_count INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_scoring_webhooks_tenant ON scoring_webhooks(tenant_id);
CREATE INDEX IF NOT EXISTS idx_scoring_webhooks_score ON scoring_webhooks(tenant_id, min_score);
