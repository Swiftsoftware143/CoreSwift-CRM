-- ONE PUBLIC ADDRESS PER WORKSPACE — a table of its own (2026-11-23).
--
-- The first attempt put a `workspace_id` on `calendar_hosts`, which cannot work: that table carries
-- `UNIQUE (tenant_id)` (calendar_hosts_tenant_id_key) because it was designed as ONE address per
-- ACCOUNT. Adding a second row for the same account therefore raised a unique violation, and the
-- handler reported it as "that address is already taken" — a false story about a real constraint.
--
-- So the per-workspace addresses get their own table, and `calendar_hosts` stays exactly what it is:
-- the account-level address every existing customer already has. Resolution tries the workspace table
-- first (more specific), then the account table, then a calendar's own subdomain.
CREATE TABLE IF NOT EXISTS workspace_hosts (
    id                      uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id               uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id            uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    subdomain               text NOT NULL,
    custom_domain           text,
    custom_domain_status    text NOT NULL DEFAULT 'none',
    custom_domain_checked_at timestamptz,
    custom_domain_last_error text,
    is_active               boolean NOT NULL DEFAULT true,
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now()
);

-- One address per workspace, and one owner per address across the WHOLE platform: the wildcard vhost
-- serves every name from the same space, so a duplicate is a tenancy defect, not an inconvenience.
CREATE UNIQUE INDEX IF NOT EXISTS workspace_hosts_workspace_key ON workspace_hosts (workspace_id);
CREATE UNIQUE INDEX IF NOT EXISTS workspace_hosts_subdomain_key ON workspace_hosts (lower(subdomain));
CREATE UNIQUE INDEX IF NOT EXISTS workspace_hosts_custom_domain_key
    ON workspace_hosts (lower(custom_domain)) WHERE custom_domain IS NOT NULL;
CREATE INDEX IF NOT EXISTS workspace_hosts_tenant_idx ON workspace_hosts (tenant_id);

COMMENT ON TABLE workspace_hosts IS
    'The public address of one WORKSPACE (a newsletter, a town, a brand). Lists only that workspace''s calendars. calendar_hosts remains the account-level address.';

-- The abandoned column on calendar_hosts, and the indexes built for it.
DROP INDEX IF EXISTS calendar_hosts_workspace_key;
DROP INDEX IF EXISTS idx_calendar_hosts_workspace;
ALTER TABLE calendar_hosts DROP COLUMN IF EXISTS workspace_id;
