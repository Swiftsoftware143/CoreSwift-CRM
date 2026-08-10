-- 021_create_event_bus_and_comms.sql
-- Centralized event bus, delayed action engine, and multi-channel communication orchestration

-- Incoming events from all external sources
CREATE TABLE IF NOT EXISTS events (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    source VARCHAR(100) NOT NULL,
    event_type VARCHAR(100) NOT NULL,
    entity_type VARCHAR(50),
    entity_id UUID,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    raw_headers JSONB,
    processed BOOLEAN DEFAULT false,
    processed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_events_tenant ON events(tenant_id);
CREATE INDEX IF NOT EXISTS idx_events_source ON events(tenant_id, source);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(tenant_id, event_type);
CREATE INDEX IF NOT EXISTS idx_events_entity ON events(tenant_id, entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_events_created ON events(tenant_id, created_at DESC);

-- "If-Not-Then" delayed action engine
CREATE TABLE IF NOT EXISTS delayed_actions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    trigger_event_id UUID REFERENCES events(id) ON DELETE SET NULL,
    condition_type VARCHAR(20) NOT NULL CHECK (condition_type IN ('timeout', 'no_event', 'no_action')),
    condition_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    action_type VARCHAR(50) NOT NULL,
    action_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    execute_at TIMESTAMPTZ NOT NULL,
    executed BOOLEAN DEFAULT false,
    cancelled BOOLEAN DEFAULT false,
    result JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_delayed_actions_tenant ON delayed_actions(tenant_id);
CREATE INDEX IF NOT EXISTS idx_delayed_actions_execute ON delayed_actions(tenant_id, execute_at) WHERE executed = false AND cancelled = false;

-- Multi-channel outbound messages
CREATE TABLE IF NOT EXISTS outbound_messages (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    channel VARCHAR(10) NOT NULL CHECK (channel IN ('email', 'sms')),
    to_address VARCHAR(255) NOT NULL,
    subject VARCHAR(500),
    body TEXT NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'sending', 'sent', 'failed')),
    sent_at TIMESTAMPTZ,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_outbound_messages_tenant ON outbound_messages(tenant_id);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_status ON outbound_messages(tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_channel ON outbound_messages(tenant_id, channel);

-- Message templates with variable substitution
CREATE TABLE IF NOT EXISTS message_templates (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    channel VARCHAR(10) NOT NULL CHECK (channel IN ('email', 'sms')),
    subject VARCHAR(500),
    body TEXT NOT NULL,
    variables JSONB DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_message_templates_tenant ON message_templates(tenant_id);

-- In-app notifications (for NotifyUser action)
CREATE TABLE IF NOT EXISTS notifications (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    message TEXT NOT NULL,
    read BOOLEAN DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_notifications_user ON notifications(tenant_id, user_id, read);
