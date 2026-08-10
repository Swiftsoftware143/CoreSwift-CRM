-- Migration 055: Private Email — named keys, key reuse, auto-responders

-- 1. Add label to private_email_domains
ALTER TABLE private_email_domains ADD COLUMN IF NOT EXISTS label VARCHAR(128);
-- Backfill existing domains with domain name as label
UPDATE private_email_domains SET label = domain WHERE label IS NULL;

-- 2. Normalize API keys into their own table (supports reuse across domains)
CREATE TABLE IF NOT EXISTS private_email_api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    label VARCHAR(128) NOT NULL,
    provider VARCHAR(32) NOT NULL DEFAULT 'mailgun',
    api_key_encrypted TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_private_email_api_keys_tenant ON private_email_api_keys(tenant_id);

-- 3. Add api_key_id FK to domains (optional — if NULL, uses the legacy mailgun_api_key column)
ALTER TABLE private_email_domains ADD COLUMN IF NOT EXISTS api_key_id UUID REFERENCES private_email_api_keys(id) ON DELETE SET NULL;

-- 4. Auto-responder / sequence rules table
CREATE TABLE IF NOT EXISTS private_email_auto_replies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    domain_id UUID NOT NULL REFERENCES private_email_domains(id) ON DELETE CASCADE,
    mailbox_id UUID REFERENCES private_email_boxes(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    trigger_type VARCHAR(32) NOT NULL,       -- 'tag_added', 'list_joined', 'pipeline_stage', 'contact_created', 'always'
    trigger_value VARCHAR(255),               -- tag name, list name, stage name, or NULL for 'always'
    subject VARCHAR(255),
    body_html TEXT NOT NULL,
    delay_minutes INTEGER NOT NULL DEFAULT 0, -- 0 = immediate
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_private_email_auto_replies_tenant ON private_email_auto_replies(tenant_id);
