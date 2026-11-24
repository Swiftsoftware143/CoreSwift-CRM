-- 209 — An issued workspace key can be READ-ONLY.
--
-- Why: `POST /api/mcp` now serves the CRM's domain reads beside the Custom Objects tools, and the
-- audience for a read door includes assistants and sibling apps that must NOT be able to write.
-- Until this column existed the only thing distinguishing one key from another was its name: every
-- `csk_` key resolved to a full workspace credential, so "give the assistant a key" meant handing
-- it the ability to archive objects, purge records and delete custom-object rows.
--
-- The role is a property of the KEY, not of the user: the same person can hold a workspace key in
-- the console and mint a read-only key for a tool. `workspace` is the default so every existing row
-- keeps exactly the authority it had (fail-safe in the permissive direction only for rows that
-- already existed and were already full-access).
--
-- The door reads this column on EVERY request (`src/mcp_door/mod.rs::Resolver::resolve_key`) and
-- refuses a read-only credential on any tool whose minimum role is `workspace` — Custom Objects
-- writes included. A platform admin's key still resolves to the operator role, which is how the
-- dashboard's own contract probe gets an admin token.
ALTER TABLE personal_api_keys
    ADD COLUMN IF NOT EXISTS api_role text NOT NULL DEFAULT 'workspace';

-- Two values today; a third ("admin") is deliberately NOT storable — operator authority comes from
-- `users.is_platform_admin`, never from a key a customer can mint.
ALTER TABLE personal_api_keys DROP CONSTRAINT IF EXISTS personal_api_keys_api_role_check;
ALTER TABLE personal_api_keys
    ADD CONSTRAINT personal_api_keys_api_role_check
    CHECK (api_role IN ('read_only', 'workspace'));

COMMENT ON COLUMN personal_api_keys.api_role IS
    'read_only: may call read tools only. workspace: the customer''s full key. Operator authority is users.is_platform_admin, never a key role.';
