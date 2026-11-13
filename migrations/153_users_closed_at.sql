-- 153_users_closed_at.sql — a customer can close their own account (2026-11-13, kanban t_ae02611a).
--
-- CoreSwift was the LAST app of the six without an exit door: the only lever was asking support to
-- DELETE the user row, and the FK chain from `tenants`/`users` CASCADEs, so a privacy request would
-- have taken the workspace's contacts, deals and history with it.
--
-- A closure is therefore a STATE, never a delete:
--   * `closed_at` records the decision, so the operator still sees the account and its data;
--   * `sessions_valid_from` is the account's SESSION LINE — the same column the other five apps
--     carry. The close handler moves it one second past NOW() in the same statement, so every token
--     minted before the closure is refused at its next request (the boundary reads the effective
--     line, see `auth::middleware::user_session_state`);
--   * `login`, `refresh`, `forgot-password` and `reset-password` refuse a closed account by name, so
--     neither a password nor a reset link can quietly reopen it;
--   * the account's live personal/satellite API keys are retired, because those are durable
--     credentials that the session line does not cover.
-- `is_active` is deliberately left alone: it is the operator's own switch, and a closed customer
-- must stay visible in the operator's people list instead of vanishing from it.
--
-- Idempotent by construction (ADD COLUMN IF NOT EXISTS / CREATE INDEX IF NOT EXISTS), so an
-- already-migrated database is untouched.
ALTER TABLE users ADD COLUMN IF NOT EXISTS closed_at timestamptz;
ALTER TABLE users ADD COLUMN IF NOT EXISTS sessions_valid_from timestamptz;

-- The operator's audits ask "who closed?" — a partial index keeps that read cheap without indexing
-- every live row.
CREATE INDEX IF NOT EXISTS idx_users_closed_at
    ON users (closed_at) WHERE closed_at IS NOT NULL;
