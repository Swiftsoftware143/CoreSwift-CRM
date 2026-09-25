-- 091_tenants_cascade_the_12_no_action_children.sql
-- t_d8d1a175: the other half of the relation 088 armed.
--
-- `tenants` has 74 children through `tenant_id`. 088 gave 7 of them the reference they never had;
-- this migration fixes the DELETE ACTION of the 12 that reference it with NO ACTION
-- (`pg_constraint.confdeltype = 'a'`) — i.e. the workspaces that could not be retired at all:
--
--   api_keys, cs_messages, email_templates, events, link_clicks, list_members, message_templates,
--   notification_rules, notifications, provider_keys, tag_assignments, tracked_links
--
-- Measured live on `coreswift_crm` 2026-09-25 (HEAD 1af5f81, 115 tenants), one throwaway workspace
-- per child, each delete inside BEGIN … ROLLBACK (evidence:
-- /opt/swift/audits/cs-tenant-delete-actions-t_d8d1a175/05-matrix-before.txt): a workspace holding
-- ONE row in ANY of the 12 cannot be deleted — all 12 raise 23503 where the same statement succeeds
-- for the 75 tenants that hold no row in any of them. Before this migration 11 of the 12 blocked
-- directly by their own constraint and `link_clicks` blocked through its parent `tracked_links`
-- (which is itself one of the 12). Live rows standing in the way, by child:
--
--   tag_assignments    461 rows / 31 tenants      notifications        3 /  1
--   list_members       105 rows / 22 tenants      provider_keys        2 /  2
--   message_templates   17 rows /  4 tenants      api_keys, cs_messages, email_templates,
--   events              12 rows / 12 tenants      link_clicks, notification_rules, tracked_links: 0
--
-- Action per table = ON DELETE CASCADE for all 12. Every one of them is tenant-owned data with no
-- platform-level meaning left behind, and for three of them the alternative is measurably worse:
--
--   provider_keys   `tenant_id` is NOT NULL and the stored secret is sealed with a key DERIVED FROM
--                   THE TENANT ID (`src/secret_box.rs`: "the AES key is derived from the SERVER
--                   SECRET in the environment + the tenant id"), so a row kept without a workspace
--                   would be undecryptable ciphertext with no owner — a liability, not data. A
--                   SET NULL arm is not even available at this schema (column is NOT NULL).
--   api_keys        `tenant_id` is NULLABLE, so SET NULL is possible — and wrong: it would leave a
--                   live credential row naming a workspace that no longer exists, which is exactly
--                   the broken-token state carded as t_524bfbb9. 0 rows live, and no reader or
--                   writer in this repo: the table carries no DDL in `migrations/` at all, so it is
--                   guarded below instead of being assumed present.
--   notifications   `tenant_id` is NOT NULL and every read path is scoped by (tenant_id, user_id)
--                   (`src/notifications/handlers.rs`): there is no platform-wide feed to preserve.
--
--   email_templates is the one table in the set with a platform-level row: `welcome / Default
--   Welcome Email` has `tenant_id IS NULL`. No delete action can touch a NULL — proven live: it is
--   still there after the rehearsal (`07-dryrun-after.txt`).
--
-- Residual, measured, and NOT changed here — the nested NO ACTION edges the cascade now walks
-- through once `tenants` cascades: `notification_rules.template_id -> message_templates`,
-- `link_clicks.tracked_link_id -> tracked_links`, `cs_messages.parent_message_id -> cs_messages`
-- (self), and (other parents) `notifications.user_id -> users`. The full-chain rehearsal proves the
-- tenant delete succeeds anyway when all of them are held at once; the remaining cross-table
-- delete endpoints are a separate relation from `tenants` and are carded, not smuggled (t_ef53b9ff:
-- 11 more NO ACTION edges hang off users/contacts/tags/message_templates/tracked_links/cs_messages,
-- 3 live rows on notifications.user_id).
--
-- Every statement below was measured on live inside BEGIN … ROLLBACK before this file was written:
-- all 12 blocked the delete before, one single `DELETE FROM tenants` removes everything after, the
-- whole-DB row-count fingerprint is identical around the migration, and all 12 live row counts are
-- unchanged (`07-dryrun-after.txt`, REPORT.md).

-- 1. The ten children that have DDL in this repository: plain, validated constraint replacement.
ALTER TABLE email_templates DROP CONSTRAINT IF EXISTS email_templates_tenant_id_fkey;
ALTER TABLE email_templates ADD CONSTRAINT email_templates_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE events DROP CONSTRAINT IF EXISTS events_tenant_id_fkey;
ALTER TABLE events ADD CONSTRAINT events_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE link_clicks DROP CONSTRAINT IF EXISTS link_clicks_tenant_id_fkey;
ALTER TABLE link_clicks ADD CONSTRAINT link_clicks_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE list_members DROP CONSTRAINT IF EXISTS list_members_tenant_id_fkey;
ALTER TABLE list_members ADD CONSTRAINT list_members_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE message_templates DROP CONSTRAINT IF EXISTS message_templates_tenant_id_fkey;
ALTER TABLE message_templates ADD CONSTRAINT message_templates_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE notification_rules DROP CONSTRAINT IF EXISTS notification_rules_tenant_id_fkey;
ALTER TABLE notification_rules ADD CONSTRAINT notification_rules_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE notifications DROP CONSTRAINT IF EXISTS notifications_tenant_id_fkey;
ALTER TABLE notifications ADD CONSTRAINT notifications_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE provider_keys DROP CONSTRAINT IF EXISTS provider_keys_tenant_id_fkey;
ALTER TABLE provider_keys ADD CONSTRAINT provider_keys_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE tag_assignments DROP CONSTRAINT IF EXISTS tag_assignments_tenant_id_fkey;
ALTER TABLE tag_assignments ADD CONSTRAINT tag_assignments_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE tracked_links DROP CONSTRAINT IF EXISTS tracked_links_tenant_id_fkey;
ALTER TABLE tracked_links ADD CONSTRAINT tracked_links_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

-- 2. `api_keys` and `cs_messages` exist live but carry NO `CREATE TABLE` in `migrations/` — a
--    database built from this repository has no such table, and a bare ALTER would abort that
--    boot. Guarded instead of assumed; that drift is carded separately (t_d8b2e888: 17 live tables,
--    this one among them, have no DDL in `migrations/`).
DO $$
BEGIN
    IF to_regclass('public.api_keys') IS NOT NULL THEN
        EXECUTE 'ALTER TABLE api_keys DROP CONSTRAINT IF EXISTS api_keys_tenant_id_fkey';
        EXECUTE 'ALTER TABLE api_keys ADD CONSTRAINT api_keys_tenant_id_fkey
                 FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE';
    ELSE
        RAISE NOTICE 't_d8d1a175: api_keys does not exist here (no DDL in migrations) - skipped';
    END IF;

    IF to_regclass('public.cs_messages') IS NOT NULL THEN
        EXECUTE 'ALTER TABLE cs_messages DROP CONSTRAINT IF EXISTS cs_messages_tenant_id_fkey';
        EXECUTE 'ALTER TABLE cs_messages ADD CONSTRAINT cs_messages_tenant_id_fkey
                 FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE';
    ELSE
        RAISE NOTICE 't_d8d1a175: cs_messages does not exist here (no DDL in migrations) - skipped';
    END IF;
END $$;
