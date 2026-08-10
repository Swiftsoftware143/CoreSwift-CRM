-- 022_create_flawless_followup.sql
-- Onboarding checklists, account health monitoring, and pre-population data

-- ============================================================
-- CHECKLIST SYSTEM
-- ============================================================

-- Onboarding checklists (staged templates + per-tenant progress)
CREATE TABLE IF NOT EXISTS checklist_templates (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    trigger_type VARCHAR(50) NOT NULL,  -- 'signup', 'trial_started', 'payment.received', 'contact.created'
    stage_count INTEGER NOT NULL DEFAULT 4,
    days_per_stage INTEGER NOT NULL DEFAULT 2,
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS checklist_stages (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    template_id UUID NOT NULL REFERENCES checklist_templates(id) ON DELETE CASCADE,
    stage_order INTEGER NOT NULL,
    title VARCHAR(255) NOT NULL,
    description TEXT,
    action_required VARCHAR(100),  -- 'logo', 'hours', 'keywords', 'verify'
    channel VARCHAR(10) DEFAULT 'email' CHECK (channel IN ('email','sms','both')),
    message_template TEXT,
    delay_hours INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS checklist_instances (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    template_id UUID NOT NULL REFERENCES checklist_templates(id),
    entity_type VARCHAR(50) NOT NULL,
    entity_id UUID NOT NULL,
    current_stage INTEGER DEFAULT 0,
    completed BOOLEAN DEFAULT false,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS checklist_progress (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    instance_id UUID NOT NULL REFERENCES checklist_instances(id) ON DELETE CASCADE,
    stage_order INTEGER NOT NULL,
    completed BOOLEAN DEFAULT false,
    action_taken VARCHAR(100),
    completed_at TIMESTAMPTZ,
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ============================================================
-- ACCOUNT HEALTH MONITORING
-- ============================================================

CREATE TABLE IF NOT EXISTS account_health (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    entity_type VARCHAR(50) NOT NULL,  -- 'tenant', 'trial_user', 'client'
    entity_id UUID NOT NULL,
    score INTEGER DEFAULT 100,          -- 100 = perfect, 0 = critical
    last_active_at TIMESTAMPTZ,
    risk_level VARCHAR(20) DEFAULT 'healthy' CHECK (risk_level IN ('healthy','at_risk','critical','churned')),
    signals JSONB DEFAULT '[]'::jsonb,  -- individual behavior signals
    last_intervention_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id, entity_type, entity_id)
);

CREATE TABLE IF NOT EXISTS health_thresholds (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    entity_type VARCHAR(50) NOT NULL,
    metric VARCHAR(100) NOT NULL,  -- 'days_inactive', 'login_frequency', 'feature_usage'
    operator VARCHAR(5) NOT NULL CHECK (operator IN ('lt','gt','eq','lte','gte')),
    value INTEGER NOT NULL,
    risk_level VARCHAR(20) NOT NULL DEFAULT 'at_risk',
    intervention_action VARCHAR(50) NOT NULL DEFAULT 'send_notification',
    intervention_config JSONB DEFAULT '{}'::jsonb,
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ============================================================
-- PRE-POPULATION DATA CACHE
-- ============================================================

CREATE TABLE IF NOT EXISTS prepopulated_data (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    source_url TEXT,
    entity_type VARCHAR(50) NOT NULL,
    entity_id UUID,
    data JSONB NOT NULL DEFAULT '{}'::jsonb,
    preview_link TEXT,
    verified BOOLEAN DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
