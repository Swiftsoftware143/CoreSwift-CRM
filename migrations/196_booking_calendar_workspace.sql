-- Cloning a calendar, and which WORKSPACE it belongs to (David, 2026-11-23).
--
-- "if a user clicks on clone a calendar this should be another thing to say which workspace as well as
-- a rename so it's just not a clone hyphen".
--
-- A calendar has always belonged to an ACCOUNT (tenant_id) and to no particular workspace, so a clone
-- had nowhere to be asked "which workspace". The column is what makes that question answerable: a
-- calendar now names the workspace it belongs to, and a clone lands in the workspace the owner picks.
--
-- Backfill is the account's DEFAULT workspace (else its oldest), which is where every existing
-- calendar has effectively been living: no calendar changes hands, and a workspace with several
-- calendars keeps them together. Nullable on purpose — a calendar created by an internal service call
-- before this column existed must not fail to load; the console treats a null as "the default
-- workspace" rather than inventing a home for it.
ALTER TABLE booking_calendars
    ADD COLUMN IF NOT EXISTS workspace_id uuid REFERENCES workspaces(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_booking_calendars_workspace
    ON booking_calendars (workspace_id);

COMMENT ON COLUMN booking_calendars.workspace_id IS
    'The workspace this calendar belongs to. A clone is created in the workspace the owner picks; NULL means the account default (never invented).';

UPDATE booking_calendars c
   SET workspace_id = w.id
  FROM (
        SELECT tenant_id, id
          FROM (
                SELECT tenant_id, id,
                       row_number() OVER (PARTITION BY tenant_id
                                          ORDER BY is_default DESC, created_at ASC) AS rn
                  FROM workspaces
               ) ranked
         WHERE ranked.rn = 1
       ) w
 WHERE c.workspace_id IS NULL
   AND w.tenant_id = c.tenant_id;
