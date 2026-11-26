-- 221_personal_api_keys_no_delete_role.sql
--
-- A THIRD KEY ROLE: read and write, never delete.
--
-- A customer's AI/MCP client has to run the business — create contacts, move deals, edit a
-- campaign — while never being able to REMOVE a contact, a record or an object. `workspace` is
-- read + write + delete and `read_only` cannot write at all, so neither expresses that. A client
-- that merely promises not to delete is not a control: the capability has to be gone from the
-- credential (kanban t_30fc7d62).
--
-- Enforced at the key, on every door: the MCP door serves such a key from a registry with no
-- destructive tool in it at all, the forwarding proxy refuses a delete-shaped call before it
-- forwards, and the REST key surface refuses DELETE before the handler runs.
--
-- Existing rows are untouched — `workspace` keeps exactly the meaning it always had.
ALTER TABLE personal_api_keys DROP CONSTRAINT IF EXISTS personal_api_keys_api_role_check;
ALTER TABLE personal_api_keys
    ADD CONSTRAINT personal_api_keys_api_role_check
    CHECK (api_role IN ('read_only', 'read_write_no_delete', 'workspace'));

COMMENT ON COLUMN personal_api_keys.api_role IS
    'read_only = reads only; read_write_no_delete = reads and writes, never deletes; workspace = reads, writes and deletes. Operator authority is users.is_platform_admin, never a key role.';
