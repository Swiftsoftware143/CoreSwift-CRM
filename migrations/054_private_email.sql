-- Migration 054: Private Email Box
-- Multi-domain, multi-mailbox per tenant with Mailgun integration

CREATE TABLE IF NOT EXISTS private_email_domains (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    domain VARCHAR(255) NOT NULL,
    mailgun_api_key TEXT NOT NULL,
    mailgun_region VARCHAR(4) NOT NULL DEFAULT 'us',
    catch_all_enabled BOOLEAN NOT NULL DEFAULT false,
    verified BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(tenant_id, domain)
);

CREATE TABLE IF NOT EXISTS private_email_boxes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    domain_id UUID NOT NULL REFERENCES private_email_domains(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    local_part VARCHAR(64) NOT NULL,
    email_address VARCHAR(255) NOT NULL,
    mailgun_mailbox_id VARCHAR(255),
    forwarding_enabled BOOLEAN NOT NULL DEFAULT true,
    signature TEXT,
    status VARCHAR(32) NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(tenant_id, email_address)
);

CREATE INDEX IF NOT EXISTS idx_private_email_domains_tenant ON private_email_domains(tenant_id);
CREATE INDEX IF NOT EXISTS idx_private_email_boxes_tenant ON private_email_boxes(tenant_id);
CREATE INDEX IF NOT EXISTS idx_private_email_boxes_domain ON private_email_boxes(domain_id);
