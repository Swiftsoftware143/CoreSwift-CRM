-- A welcome mail is sent ONCE per company, ever.
--
-- The CEO's standard is explicit (2026-11-15): a re-sync "never re-sends mail for a company that already has an
-- account". Asking for the mail is not enough of a guard — a script run twice would mail twice, and the owner
-- would get the same credentials twice with no way to tell which was real. This column is the ledger: NULL means
-- nobody has been told yet, and the provisioning pass sets it the moment the mail is queued.
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS welcome_mailed_at timestamptz;
