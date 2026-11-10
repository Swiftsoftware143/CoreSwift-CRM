-- 124_workspaces.sql
-- Workspaces: a tenant (business) can hold many workspaces; each workspace owns its
-- own contacts, mailboxes, automations, calendars. Settings stay shared at the tenant.
-- Additive only: no existing table is altered here.

CREATE TABLE IF NOT EXISTS workspaces (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        VARCHAR(120) NOT NULL,
    is_default  BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- two workspaces in one account may not share a name
CREATE UNIQUE INDEX IF NOT EXISTS ux_workspaces_tenant_name
    ON workspaces (tenant_id, lower(name));

CREATE INDEX IF NOT EXISTS ix_workspaces_tenant
    ON workspaces (tenant_id);

-- exactly one default workspace per tenant (the one a fresh session opens)
CREATE UNIQUE INDEX IF NOT EXISTS ux_workspaces_one_default
    ON workspaces (tenant_id) WHERE is_default;

-- every existing account gets its default workspace so nothing is blank
INSERT INTO workspaces (tenant_id, name, is_default)
SELECT t.id, 'My Workspace', true
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM workspaces w WHERE w.tenant_id = t.id);
