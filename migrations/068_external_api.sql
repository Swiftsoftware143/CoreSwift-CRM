-- 068: External API — per-user personal API keys + auto-provisioned contact custom fields.
-- Enables third-party apps (e.g. IncentiveSwift) to push leads into a tenant's CRM
-- using a per-user key (Zapier-style), mapping data points onto contacts and
-- auto-creating named custom fields for anything that doesn't already exist.

-- Personal API keys (a tenant/user generates, then pastes into the third-party app)
CREATE TABLE IF NOT EXISTS personal_api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    name TEXT NOT NULL DEFAULT 'default',
    key_hash TEXT NOT NULL,                -- sha256 hex of the full key
    key_prefix TEXT NOT NULL DEFAULT '',   -- first 8 chars, for UI display
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_personal_api_keys_hash ON personal_api_keys(key_hash);
CREATE INDEX IF NOT EXISTS idx_personal_api_keys_tenant ON personal_api_keys(tenant_id);

-- Custom fields (auto-provisioned per tenant when an unknown data point arrives)
CREATE TABLE IF NOT EXISTS contact_custom_fields (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    key TEXT NOT NULL,                      -- normalized field key (snake_case)
    label TEXT NOT NULL,                    -- human label (the source question/field name)
    field_type TEXT NOT NULL DEFAULT 'text',-- text | number | boolean | date | url
    source_app TEXT NOT NULL DEFAULT 'external', -- which app created it (e.g. 'incentiveswift')
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, key)
);
CREATE INDEX IF NOT EXISTS idx_contact_custom_fields_tenant ON contact_custom_fields(tenant_id);

-- Values for custom fields per contact (EAV)
CREATE TABLE IF NOT EXISTS contact_field_values (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contact_id UUID NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    field_id UUID NOT NULL REFERENCES contact_custom_fields(id) ON DELETE CASCADE,
    value TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (contact_id, field_id)
);
CREATE INDEX IF NOT EXISTS idx_contact_field_values_contact ON contact_field_values(contact_id);
CREATE INDEX IF NOT EXISTS idx_contact_field_values_field ON contact_field_values(field_id);
