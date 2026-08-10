-- Migration 056: Email provider types — support Mailgun, SMTP, SES, Postmark
-- Each domain can choose its delivery + inbound provider independently.

-- 1. Add provider_type to private_email_domains
ALTER TABLE private_email_domains 
  ADD COLUMN IF NOT EXISTS provider_type VARCHAR(32) NOT NULL DEFAULT 'mailgun';

-- 2. Add SMTP provider config columns (nullable, only used when provider_type='smtp')
ALTER TABLE private_email_domains 
  ADD COLUMN IF NOT EXISTS smtp_host VARCHAR(255),
  ADD COLUMN IF NOT EXISTS smtp_port INTEGER DEFAULT 587,
  ADD COLUMN IF NOT EXISTS smtp_username VARCHAR(255),
  ADD COLUMN IF NOT EXISTS smtp_password_encrypted TEXT,  -- encrypted at rest
  ADD COLUMN IF NOT EXISTS smtp_tls BOOLEAN NOT NULL DEFAULT true;

-- 3. Add inbound_mode to control how inbound mail is received
-- 'webhook' = provider POSTs to our endpoint (Mailgun, SES, Postmark)
-- 'none'     = inbound not configured
-- 'imap'     = future: poll a mailbox
ALTER TABLE private_email_domains 
  ADD COLUMN IF NOT EXISTS inbound_mode VARCHAR(16) NOT NULL DEFAULT 'webhook';

-- 4. Create provider_api_keys table for BYO API providers (SES, Postmark)
-- Generic replacement for the mailgun-specific key columns
CREATE TABLE IF NOT EXISTS provider_api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    label VARCHAR(128) NOT NULL,
    provider VARCHAR(32) NOT NULL,  -- 'mailgun', 'ses', 'postmark'
    access_key_encrypted TEXT,       -- encrypted at rest
    secret_key_encrypted TEXT,       -- encrypted at rest (for SES)
    region VARCHAR(16),             -- AWS region for SES, 'us'/'eu' for mailgun
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_provider_api_keys_tenant ON provider_api_keys(tenant_id);

-- 5. Add provider_api_key_id FK to domains (replaces api_key_id from mig 055 for non-mailgun)
-- Already have api_key_id UUID column from mig 055, but it points to private_email_api_keys.
-- We reuse it: it can point to either table based on provider_type.
-- For backward compat: if provider_type='mailgun', api_key_id references private_email_api_keys.
-- For provider_type IN ('ses','postmark'), api_key_id references provider_api_keys.

-- 6. Webhook signing keys per provider (for inbound verification)
ALTER TABLE private_email_domains 
  ADD COLUMN IF NOT EXISTS webhook_signing_key_encrypted TEXT;
