-- kanban t_6099a407 (class guard): EVERY tenant gains its default workspace at creation.
--
-- The account writer (`auth::signup::create_account`) now inserts the first workspace inside its
-- own transaction, so the two customer doors (the public signup and
-- `POST /api/v1/internal/provision-free-account`) arrive complete. But `tenants` has more writers
-- than those two: the agency sub-account door (`account::handlers::create`), the admin console
-- (`admin_actions`), the portfolio sync (`portfolio::handlers`) and the cross-app tag webhook
-- (`webhooks::cross_app_tag_sync`) each mint a tenant + its plan row and no workspace — measured
-- in this tree on 2026-11-11 (`grep -rn 'INSERT INTO tenants' src/`). Fixing one writer per card is
-- how the blank state comes back through the next door.
--
-- So the rule lives where a tenant is created, once: an AFTER INSERT trigger gives the new tenant
-- its default workspace. It is the same shape the writer and migration 133 use — the account's own
-- name, clipped to `workspaces.name` (`VARCHAR(120)`), `is_default = true` — and it is guarded on
-- "no workspace yet" so it can never fight a writer that already inserted one (the signup's own
-- insert runs first inside its transaction, so the trigger is a no-op there).
--
-- Idempotent: `CREATE OR REPLACE` + `DROP TRIGGER IF EXISTS` means re-running this file (or a
-- from-zero build applying it once) always lands the same single trigger.

CREATE OR REPLACE FUNCTION tenants_default_workspace() RETURNS trigger
LANGUAGE plpgsql AS $fn$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM workspaces w WHERE w.tenant_id = NEW.id) THEN
        INSERT INTO workspaces (tenant_id, name, is_default)
        VALUES (
            NEW.id,
            left(coalesce(nullif(btrim(NEW.name), ''), 'My Workspace'), 120),
            true
        )
        ON CONFLICT DO NOTHING;
    END IF;
    RETURN NULL;
END;
$fn$;

DROP TRIGGER IF EXISTS trg_tenants_default_workspace ON tenants;

CREATE TRIGGER trg_tenants_default_workspace
    AFTER INSERT ON tenants
    FOR EACH ROW
    EXECUTE FUNCTION tenants_default_workspace();
