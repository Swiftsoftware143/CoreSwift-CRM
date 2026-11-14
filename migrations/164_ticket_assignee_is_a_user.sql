-- Assignment means a teammate.
--
-- `tickets.assigned_to` was created pointing at `contacts(id)` — the CRM's customer table — so the one
-- thing a support queue exists to do, giving a ticket to a person on the team, was impossible: every
-- attempt answered 500 on the foreign key. Found while proving the ticket views, where "My open" could
-- never contain anything.
--
-- The inbox already gets this right: `private_email_messages.assigned_user_id` references `users(id)`.
-- This points the ticket at the same table, so the two views of one conversation agree about who owns it.
--
-- Safe to change now: no ticket carries an owner (every row is NULL). The guard below refuses the change
-- anyway if any row ever does point at something that is not a user, so this cannot silently orphan data.

ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_assigned_to_fkey;

DO $$
DECLARE
    stranded integer;
BEGIN
    SELECT count(*) INTO stranded
      FROM tickets t
     WHERE t.assigned_to IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM users u WHERE u.id = t.assigned_to);
    IF stranded > 0 THEN
        RAISE EXCEPTION
            'refusing to repoint tickets.assigned_to: % row(s) name a contact, not a user', stranded;
    END IF;
END $$;

ALTER TABLE tickets
  ADD CONSTRAINT tickets_assigned_to_fkey
  FOREIGN KEY (assigned_to) REFERENCES users(id) ON DELETE SET NULL;

-- The views an agent works from all filter on the owner, so the index follows the new column meaning.
DROP INDEX IF EXISTS idx_tickets_open_assigned;
CREATE INDEX IF NOT EXISTS idx_tickets_open_assigned
    ON tickets (tenant_id, assigned_to)
    WHERE status IN ('new', 'open', 'pending', 'on_hold');
