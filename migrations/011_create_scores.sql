-- 011_create_scores.sql
-- Contact scores and score history

CREATE TABLE IF NOT EXISTS contact_scores (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    contact_id UUID NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    total_score INTEGER NOT NULL DEFAULT 0,
    category VARCHAR(20) NOT NULL DEFAULT 'cold',
    calculated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(contact_id)
);

CREATE INDEX IF NOT EXISTS idx_contact_scores_tenant ON contact_scores(tenant_id);
CREATE INDEX IF NOT EXISTS idx_contact_scores_score ON contact_scores(tenant_id, total_score DESC);

CREATE TABLE IF NOT EXISTS score_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    contact_id UUID NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    rule_id UUID REFERENCES score_rules(id) ON DELETE SET NULL,
    event_type VARCHAR(100),
    points INTEGER NOT NULL,
    previous_score INTEGER NOT NULL DEFAULT 0,
    new_score INTEGER NOT NULL DEFAULT 0,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_score_history_contact ON score_history(contact_id);
CREATE INDEX IF NOT EXISTS idx_score_history_time ON score_history(created_at);
