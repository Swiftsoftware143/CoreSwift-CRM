-- 141_team_seat_invitations.sql
-- Team seats: who was invited, by whom, and when.
--
-- David 2026-11-11 (kanban t_5f314e9a): a real SECOND LOGIN into the account. The owner invites a
-- teammate by email, the system generates the password and mails it, and the teammate signs in to
-- the shared inbox. Two columns record WHO added a person and WHEN, so the roster can say "invited
-- by the owner on <date>" instead of pretending every seat was a self-serve signup.
--
-- Nothing here changes an existing row's meaning: both columns are NULL for every user that
-- predates this migration (every real owner, and every self-serve signup), which is exactly what
-- "not invited by somebody else" means.
--
-- invitations do not need their own table: the seat IS the users row (one writer, one object), and
-- an invitation that is not yet a row is what the mail is for. The link-based `tenant_invites` table
-- keeps working untouched.
ALTER TABLE users ADD COLUMN IF NOT EXISTS invited_by uuid REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE users ADD COLUMN IF NOT EXISTS invited_at timestamptz;

-- The seat ceiling counts ACTIVE users of one workspace on every invite, and every member list
-- reads the same predicate. A partial index keeps those two counts off a sequential scan as the
-- users table grows across workspaces.
CREATE INDEX IF NOT EXISTS idx_users_tenant_active ON users (tenant_id) WHERE is_active;
