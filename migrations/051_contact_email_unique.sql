-- Email is the identifier — unique per tenant when not null
CREATE UNIQUE INDEX IF NOT EXISTS idx_contacts_tenant_email 
ON contacts(tenant_id, email) WHERE email IS NOT NULL;
