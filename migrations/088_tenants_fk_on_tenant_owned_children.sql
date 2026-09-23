-- 088_tenants_fk_on_tenant_owned_children.sql
-- Give the 7 tenant_id columns whose writers already assume a workspace the reference they
-- never had (card t_cc55a489).
--
-- Measured live on `coreswift_crm` 2026-09-23: `tenants` is a parent of 74 tables through a
-- `tenant_id` column, and these 7 carried NO foreign key back to it, so `DELETE FROM tenants`
-- silently ORPHANED their rows instead of cascading them:
--
--   account_health, business_profiles, delayed_actions, notification_queue,
--   outbound_messages, portfolio_companies, tickets
--
-- The card named 8 tables and listed `scores`; that is wrong and this migration does not touch
-- it — `scores` already carries `contact_scores_tenant_id_fkey FOREIGN KEY (tenant_id)
-- REFERENCES tenants(id) ON DELETE CASCADE`, validated (pg_constraint.convalidated = true). The
-- asymmetry is a per-migration accident, not a design decision: `opportunities`, `contacts`,
-- `tenant_invites`, `tenant_plans`, `users`, `scores` all cascade.
--
-- Consequence measured in t_ecdfcff2: retiring 24 orphan workspaces left 7 `outbound_messages`
-- rows behind, which a human had to remove with a second guarded DELETE inside the same
-- transaction. The defect also left a backlog of rows already dangling today:
--
--   outbound_messages    383 rows / 359 distinct deleted tenant ids (2026-08-09 .. 2026-09-23)
--   portfolio_companies    3 rows /   1 deleted tenant id (869c33e6…, the demo portfolio)
--   the other five           0 dangling rows
--
-- and 1 `business_profiles` row with `tenant_id IS NULL` (written by
-- `src/webhook/actions.rs` without a tenant). A NULL is not a violation — the FK permits it, and
-- that row is left untouched.
--
-- Action per table = ON DELETE CASCADE for all 7. Every one of them is tenant-owned data with no
-- platform-level meaning left behind: account_health is the monitoring engine's per-entity health
-- snapshot, business_profiles the tenant's own business profile, delayed_actions the tenant's
-- scheduled follow-ups, notification_queue and outbound_messages the tenant's own queue/log,
-- portfolio_companies the tenant's portfolio (its `integration_targets` cascade from it),
-- tickets the tenant's support tickets (their `ticket_messages` cascade from it). Nothing is
-- left for a RESTRICT/SET NULL arm to protect, and no incoming reference to these 7 tables is
-- NO ACTION/RESTRICT, so cascading into them cannot turn a working tenant delete into a failure:
-- measured on live, the only non-cascade edges reachable from `tenants` are on other tables
-- (`api_keys`, `cs_messages`, `email_templates`, `events`, `link_clicks`, `list_members`,
-- `message_templates`, `notification_rules`, `notifications`, `provider_keys`,
-- `tag_assignments`, `tracked_links`, `checklist_instances`, `opportunities`) and were already
-- unvalidated NO ACTION before this migration.
--
-- `outbound_messages` and `portfolio_companies` are armed NOT VALID on purpose: they still hold
-- the 383 + 3 historical dangling rows above, so a validating ADD CONSTRAINT would abort. NOT
-- VALID is not a no-op — Postgres enforces it against every subsequent INSERT/UPDATE (proven on
-- live: an insert naming a nonexistent tenant_id is refused with 23503), so the defect stops
-- here. Removing those 386 rows and then VALIDATE CONSTRAINT is carded separately (t_9a4ac01d);
-- no historical row is deleted by this migration, and every row count is unchanged by it.
--
-- Every statement below was measured on live inside BEGIN … ROLLBACK before this file was
-- written (ON_ERROR_STOP=1): the tenant delete cascaded all 7 child rows away where the same
-- rehearsal had shown all 7 surviving, a dangling insert was refused with 23503, and the whole-DB
-- row-count fingerprint was identical before and after.

-- 1. The five tables with no dangling rows: validated cascade on live data.
ALTER TABLE account_health DROP CONSTRAINT IF EXISTS account_health_tenant_id_fkey;
ALTER TABLE account_health ADD CONSTRAINT account_health_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE business_profiles DROP CONSTRAINT IF EXISTS business_profiles_tenant_id_fkey;
ALTER TABLE business_profiles ADD CONSTRAINT business_profiles_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE delayed_actions DROP CONSTRAINT IF EXISTS delayed_actions_tenant_id_fkey;
ALTER TABLE delayed_actions ADD CONSTRAINT delayed_actions_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE notification_queue DROP CONSTRAINT IF EXISTS notification_queue_tenant_id_fkey;
ALTER TABLE notification_queue ADD CONSTRAINT notification_queue_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_tenant_id_fkey;
ALTER TABLE tickets ADD CONSTRAINT tickets_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE;

-- 2. The two tables that still hold historical orphan rows: armed NOT VALID, enforced from the
--    first row written after this migration. t_9a4ac01d deletes the 386 rows and validates.
ALTER TABLE outbound_messages DROP CONSTRAINT IF EXISTS outbound_messages_tenant_id_fkey;
ALTER TABLE outbound_messages ADD CONSTRAINT outbound_messages_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE portfolio_companies DROP CONSTRAINT IF EXISTS portfolio_companies_tenant_id_fkey;
ALTER TABLE portfolio_companies ADD CONSTRAINT portfolio_companies_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE NOT VALID;
