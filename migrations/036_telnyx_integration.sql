-- 036_telnyx_integration.sql
-- Telnyx SMS/Voice integration tables and endpoints

-- Telnyx global config (system-wide, for tenants without BYOK)
CREATE TABLE IF NOT EXISTS telnyx_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    api_key TEXT NOT NULL DEFAULT '',
    profile_id VARCHAR(255),
    messaging_profile_id VARCHAR(255),
    webhook_secret VARCHAR(255),
    is_active BOOLEAN DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Telnyx phone numbers (per-tenant)
CREATE TABLE IF NOT EXISTS telnyx_numbers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    phone_number VARCHAR(20) NOT NULL,
    friendly_name VARCHAR(255),
    provider VARCHAR(32) NOT NULL DEFAULT 'telnyx',
    capabilities JSONB DEFAULT '{"sms": true, "voice": true, "mms": true}',
    is_active BOOLEAN DEFAULT true,
    telnyx_connection_id VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(phone_number, is_active)
);

CREATE INDEX IF NOT EXISTS idx_telnyx_numbers_tenant ON telnyx_numbers(tenant_id);
CREATE INDEX IF NOT EXISTS idx_telnyx_numbers_phone ON telnyx_numbers(phone_number);

-- Inbound calls (from Telnyx webhook)
CREATE TABLE IF NOT EXISTS inbound_calls (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    caller_number VARCHAR(20) NOT NULL,
    caller_name VARCHAR(255),
    called_number VARCHAR(20) NOT NULL,
    call_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    disposition VARCHAR(20) NOT NULL DEFAULT 'missed',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_inbound_calls_tenant ON inbound_calls(tenant_id);

-- Call logs (for billing/audit)
CREATE TABLE IF NOT EXISTS call_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    caller_number VARCHAR(20) NOT NULL,
    called_number VARCHAR(20) NOT NULL,
    duration INTEGER,
    disposition VARCHAR(20) NOT NULL DEFAULT 'missed',
    cost DECIMAL(10,2),
    recorded BOOLEAN DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_call_logs_tenant ON call_logs(tenant_id);

-- Add credit_balance and lifetime_credits to tenant_plans if missing
ALTER TABLE tenant_plans ADD COLUMN IF NOT EXISTS credit_balance INTEGER DEFAULT 0;
ALTER TABLE tenant_plans ADD COLUMN IF NOT EXISTS lifetime_credits INTEGER DEFAULT 0;
