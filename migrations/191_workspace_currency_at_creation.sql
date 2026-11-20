-- Every workspace is a US-dollar workspace unless it states otherwise — seated at CREATION, not just read.
--
-- David's ruling (2026-11-20): "any time a workspace gets created it should be USD, and the workspaces
-- already created should default to USD." Migration 190 seats USD on the workspaces that exist; this
-- closes the other half — a workspace created tomorrow — and re-runs 190's seating here so the rule holds
-- even on a database where 190 was never applied.
--
-- WHY BOTH A TRIGGER AND THE CODE DEFAULT: the read-time standing default in the deal writer means an
-- unstamped workspace still *behaves* as dollars, and that must stay — it is what makes the rule true even
-- if a write path forgets. But a rule that only exists at read time is invisible: the settings screen shows
-- an empty currency, the API answers `currency: null`, and a deal write refused by an older binary looks
-- like a customer's problem. Seating the value makes the workspace's own record say what it is.
--
-- REPORTING CURRENCY IS NOT WRITTEN, deliberately: it is derived from the default, and two stored copies
-- disagree the moment a workspace changes one of them.
--
-- IDEMPOTENT, and never overwrites a choice: a workspace that has stated EUR stays EUR. Non-object
-- `settings` (one probe tenant carries a JSON string) are left alone rather than coerced.

-- ── 1. the workspaces that already exist ────────────────────────────────────────────────────────────────
UPDATE tenants
   SET settings = jsonb_set(settings, '{default_currency}', to_jsonb('USD'::text), true),
       updated_at = NOW()
 WHERE jsonb_typeof(settings) = 'object'
   AND COALESCE(btrim(settings->>'default_currency'), '') = '';

UPDATE tenants
   SET settings = jsonb_build_object('default_currency', 'USD'),
       updated_at = NOW()
 WHERE settings IS NULL
    OR jsonb_typeof(settings) <> 'object';

-- ── 2. every workspace created from now on ──────────────────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION seat_default_currency() RETURNS trigger AS $$
BEGIN
    IF NEW.settings IS NULL OR jsonb_typeof(NEW.settings) <> 'object' THEN
        NEW.settings := jsonb_build_object('default_currency', 'USD');
    ELSIF COALESCE(btrim(NEW.settings->>'default_currency'), '') = '' THEN
        NEW.settings := jsonb_set(NEW.settings, '{default_currency}', to_jsonb('USD'::text), true);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_seat_default_currency ON tenants;
CREATE TRIGGER trg_seat_default_currency
    BEFORE INSERT ON tenants
    FOR EACH ROW
    EXECUTE FUNCTION seat_default_currency();
