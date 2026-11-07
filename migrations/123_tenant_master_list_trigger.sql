-- Migration 123: the Master-list trigger every live CoreSwift workspace already has.
--
-- Card t_05c25642. deploy-app.sh step 0a (from-zero baseline) measured on an UNCHANGED tree:
--   CLASS coreswift routines missing=1 extra=0
--   CLASS coreswift triggers missing=1 extra=0
--   MISSING R ensure_master_list()
--   MISSING G tenants:trigger_auto_create_master_list:AFTER
-- i.e. the live database carries one routine and one AFTER INSERT trigger on tenants that NO
-- file in migrations/ ever declared, so a database built from zero behaved differently from live:
-- a NEW workspace got no default list.
--
-- Direction: KEEP the live behaviour and make it reproducible (do NOT retire the live object).
--   * The trigger's own comment states the product intent: every workspace starts with one list so
--     the user never faces an empty Contacts screen and never has to create a list before
--     capturing a lead.
--   * `grep -rn 'Master list' src/ migrations/` returns NOTHING, so no Rust handler, no other
--     migration and no seed creates it on signup: this trigger is the ONLY mechanism that gives a
--     new workspace its default list. Retiring it would remove that behaviour from live.
--   * Measured on live: 7 of 9 workspaces carry a `lists` row named 'Master list' (the two without
--     are the tenancy canaries, whose probe rows are swept), so this is running data, not residue.
--
-- Shape follows the repo's own precedent for a tenant-bootstrap trigger (027/094/101's
-- `auto_create_webhook`): CREATE OR REPLACE FUNCTION, then DROP TRIGGER IF EXISTS + CREATE TRIGGER.
-- DDL is transactional, so the drop/create pair is atomic and no tenant INSERT can land in between.
--
-- On live this file is a NO-OP by construction: the function already exists (replaced with the same
-- body) and the trigger is dropped and recreated with the same definition — no rows are written, no
-- tenant is touched. On a from-zero build it is what makes `tenants` provision the Master list, so
-- the fresh schema and live become identical in the routines and triggers classes.
--
-- Applied by sqlx::migrate! (checksummed): never edit this file once it is applied.

CREATE OR REPLACE FUNCTION ensure_master_list()
RETURNS TRIGGER AS $$
BEGIN
    -- Every workspace starts with one list so the user never faces an empty Contacts screen and
    -- never has to create a list before capturing a lead. Guarded so a workspace created with an
    -- explicit Master list cannot end up with two.
    IF NOT EXISTS (
        SELECT 1 FROM lists WHERE tenant_id = NEW.id AND name = 'Master list'
    ) THEN
        INSERT INTO lists (id, tenant_id, name, description, list_type, is_active, created_at, updated_at)
        VALUES (gen_random_uuid(), NEW.id, 'Master list',
                'Default list - every contact lands here until you segment them',
                'static', true, now(), now());
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trigger_auto_create_master_list ON tenants;
CREATE TRIGGER trigger_auto_create_master_list
    AFTER INSERT ON tenants
    FOR EACH ROW
    EXECUTE FUNCTION ensure_master_list();
