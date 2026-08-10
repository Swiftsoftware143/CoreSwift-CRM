-- Inbound Webhook Receiver
-- Receives data pushed from satellite apps (FunnelSwift, IncentiveSwift, WorkflowSwift, MissedCall Respondr)

-- API keys table: satellite app users connect with their own credentials
CREATE TABLE IF NOT EXISTS satellite_api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    name TEXT NOT NULL DEFAULT 'default',
    source_app TEXT NOT NULL DEFAULT 'unknown',
    key_hash TEXT NOT NULL,
    key_prefix TEXT NOT NULL DEFAULT '',
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_satellite_api_keys_tenant ON satellite_api_keys(tenant_id);
CREATE INDEX IF NOT EXISTS idx_satellite_api_keys_prefix ON satellite_api_keys(key_prefix);

-- Inbound webhook event log: records every event pushed from satellites
CREATE TABLE IF NOT EXISTS inbound_webhook_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    source_app TEXT NOT NULL,
    event_type TEXT NOT NULL,
    event_payload JSONB NOT NULL DEFAULT '{}',
    api_key_id UUID REFERENCES satellite_api_keys(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'received'
        CHECK (status IN ('received', 'processed', 'failed', 'ignored')),
    error_message TEXT,
    processed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_tenant ON inbound_webhook_events(tenant_id);
CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_source ON inbound_webhook_events(tenant_id, source_app);
CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_status ON inbound_webhook_events(tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_created ON inbound_webhook_events(tenant_id, created_at DESC);

-- Tenant-level integration config (extends integration_config JSONB)
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS allowed_sources TEXT[] DEFAULT '{}';

-- Enable API key auth mode per key
ALTER TABLE satellite_api_keys ADD COLUMN IF NOT EXISTS permissions JSONB DEFAULT '["read","write"]';
