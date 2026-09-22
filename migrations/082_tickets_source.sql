-- 082_tickets_source.sql  —  kanban t_c76d941e
--
-- WHY: `083_ticket_portal.sql` widens `tickets_source_check` to accept 'portal' (so the CS-22
-- customer support portal can insert a submission), but `060_tickets.sql` — the only migration
-- that creates the table — does not create a `source` column at all. On the live database the
-- column had been added out-of-band, which is why 083 worked there and dies on a fresh install:
--     ERROR: column "source" does not exist
-- The column is added here, between the table's creation (060) and its consumer (083), in the
-- live definition: `source text NOT NULL DEFAULT 'manual'` (information_schema, 2026-09-22).
--
-- WHY VERSION 082: it has to sort after 060 and before 083; 082 is one of the two free version
-- slots in this directory (the other is 037, see 037_fresh_install_base_tables.sql).
--
-- ON THE LIVE DATABASE THIS IS A NO-OP (`ADD COLUMN IF NOT EXISTS` against a column that is
-- already there with the same type, nullability and default).

ALTER TABLE tickets ADD COLUMN IF NOT EXISTS source TEXT NOT NULL DEFAULT 'manual';
