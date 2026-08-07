-- 059_deal_reminders_timeline.sql
-- Deal reminders and timeline support for opportunities

-- Reminders table
CREATE TABLE IF NOT EXISTS deal_reminders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    opportunity_id UUID NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    remind_at TIMESTAMPTZ NOT NULL,
    reminder_type TEXT NOT NULL DEFAULT 'manual',
    note TEXT,
    is_dismissed BOOLEAN NOT NULL DEFAULT false,
    dismissed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_deal_reminders_tenant ON deal_reminders(tenant_id);
CREATE INDEX IF NOT EXISTS idx_deal_reminders_opportunity ON deal_reminders(opportunity_id);
CREATE INDEX IF NOT EXISTS idx_deal_reminders_user ON deal_reminders(user_id);
CREATE INDEX IF NOT EXISTS idx_deal_reminders_remind_at ON deal_reminders(remind_at) WHERE NOT is_dismissed;
