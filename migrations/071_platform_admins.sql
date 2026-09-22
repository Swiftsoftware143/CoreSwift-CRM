-- Platform-admin authority (kanban t_d5cf6cad).
--
-- `users.role` is ACCOUNT-scoped: registration writes 'owner' for the first user of every tenant,
-- so 38 of 47 production users (measured 2026-09-22) passed a cross-tenant gate written as
-- `role != 'owner' && role != 'agency_admin'`. Platform authority is therefore an explicit flag on
-- the user row, read from the database by the auth layer from the token SUBJECT — never from a
-- role string carried in the token.
--
-- Idempotent: safe to re-run, and safe on a database where the column already exists.

ALTER TABLE users ADD COLUMN IF NOT EXISTS is_platform_admin boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN users.is_platform_admin IS
  'May act ACROSS tenants (platform operator). Source of truth for /api/admin/*; independent of the tenant-scoped users.role.';

-- Seed the operators that already exist. In this schema the platform-operations role is
-- `agency_admin` (the plans registry, private-email admin and native-apps global config all gate
-- on it) and it cannot be self-assigned through the API: registration writes 'owner', and
-- tenant_invites.role is CHECK-constrained to admin|member.
UPDATE users
   SET is_platform_admin = true
 WHERE role = 'agency_admin'
   AND is_platform_admin IS DISTINCT FROM true;

CREATE INDEX IF NOT EXISTS idx_users_is_platform_admin
    ON users (is_platform_admin)
 WHERE is_platform_admin;
