-- kanban t_6099a407: every account owns its default workspace.
--
-- Migration 124 created `workspaces` and backfilled the tenants that existed then ("every existing
-- account gets its default workspace so nothing is blank"). The account writer
-- (`auth::signup::create_account`, the ONE mint both the signup door and the tag door call) now
-- writes the first workspace inside the same transaction, so no NEW account starts blank. This file
-- closes the remaining gap on databases that already hold blank tenants -- accounts created between
-- 124 and the writer fix (on LIVE: "Tenancy Canary A"/"Tenancy Canary B", the standing
-- tenancy-canary fixtures created 2026-11-05, and any tenant a future machine door mints without a
-- workspace).
--
-- The name is the account's own name, exactly the rule the writer applies: `tenants.name` allows
-- 255 characters while `workspaces.name` allows 120, so the label is clipped in CHARACTERS
-- (`left`) and a blank/whitespace account name falls back to the same `My Workspace` label the
-- writer uses. Both are `is_default = true`: a tenant with no workspace can only have this one, so
-- `ux_workspaces_one_default` (one default per tenant) cannot be violated.
--
-- Idempotent by construction: tenants that already hold a workspace are not touched, so re-running
-- this file on a boot is a no-op.

INSERT INTO workspaces (tenant_id, name, is_default)
SELECT t.id, left(coalesce(nullif(btrim(t.name), ''), 'My Workspace'), 120), true
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM workspaces w WHERE w.tenant_id = t.id)
ON CONFLICT DO NOTHING;
