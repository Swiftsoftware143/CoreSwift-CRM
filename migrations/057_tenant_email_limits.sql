-- Migration 057: Tenant email limits — per-tenant overrides for plan email limits
-- Allows super_admin to set custom limits per tenant that override plan defaults.

CREATE TABLE IF NOT EXISTS tenant_email_limits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL UNIQUE REFERENCES tenants(id) ON DELETE CASCADE,
    max_domains INTEGER,        -- NULL = use plan default
    max_mailboxes INTEGER,      -- NULL = use plan default
    max_aliases_per_mailbox INTEGER, -- NULL = use plan default
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_tenant_email_limits_tenant ON tenant_email_limits(tenant_id);
