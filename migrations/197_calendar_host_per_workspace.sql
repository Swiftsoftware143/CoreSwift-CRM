-- ONE CALENDAR ADDRESS PER WORKSPACE (David, 2026-11-23).
--
-- "each workspace might have multiple calendars so they need to be able to have their sub domain".
--
-- Until now `calendar_hosts` held exactly ONE row per ACCOUNT, so an account with a calendar per
-- newsletter had a single address for all of them. The column below lets an address belong to a
-- WORKSPACE; a host row with a NULL workspace_id keeps its current meaning (the account-level address
-- every existing customer already has), so nothing that works today changes address.
--
-- The wildcard plumbing already exists and needs nothing per workspace: nginx answers any
-- `<label>.coreswiftcrm.com` with the hosted calendar page (sites/calendar-coreswift.conf) and
-- /opt/swift/ssl/origin.pem carries DNS:*.coreswiftcrm.com, so a new address is https end to end the
-- moment this row exists.
ALTER TABLE calendar_hosts
    ADD COLUMN IF NOT EXISTS workspace_id uuid REFERENCES workspaces(id) ON DELETE CASCADE;

-- One address per workspace, enforced by the database rather than by a check-then-insert in a handler.
CREATE UNIQUE INDEX IF NOT EXISTS calendar_hosts_workspace_key
    ON calendar_hosts (workspace_id) WHERE workspace_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_calendar_hosts_workspace ON calendar_hosts (workspace_id);

COMMENT ON COLUMN calendar_hosts.workspace_id IS
    'The workspace this public address serves. NULL = the account-level address (one per account, unchanged). A workspace host lists only that workspace''s calendars.';
