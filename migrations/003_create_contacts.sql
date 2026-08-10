-- 003_create_contacts.sql
-- Contact records with custom fields and search support

CREATE TABLE IF NOT EXISTS contacts (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    first_name VARCHAR(100) NOT NULL,
    last_name VARCHAR(100) NOT NULL,
    email VARCHAR(255),
    phone VARCHAR(50),
    company VARCHAR(255),
    job_title VARCHAR(255),
    city VARCHAR(100),
    state VARCHAR(100),
    country VARCHAR(100),
    source VARCHAR(100),
    metadata JSONB DEFAULT '{}'::jsonb,
    score INTEGER DEFAULT 0,
    score_category VARCHAR(20) DEFAULT 'cold',
    is_active BOOLEAN DEFAULT true,
    last_contacted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_contacts_tenant_id ON contacts(tenant_id);
CREATE INDEX IF NOT EXISTS idx_contacts_email ON contacts(email);
CREATE INDEX IF NOT EXISTS idx_contacts_score ON contacts(tenant_id, score);
CREATE INDEX IF NOT EXISTS idx_contacts_source ON contacts(tenant_id, source);
CREATE INDEX IF NOT EXISTS idx_contacts_name ON contacts(tenant_id, last_name, first_name);
CREATE INDEX IF NOT EXISTS idx_contacts_created_at ON contacts(tenant_id, created_at);
