-- 099: mark harness-created tenants at creation, so a probe root is a DATUM and not a guess
-- (fleet policy /opt/swift/docs/fleet-probe-residue-policy-2026-09-28.md, answers 3(b) and 3(c);
-- kanban t_66db3251).
--
-- WHY: CoreSwift-CRM is the CRM hub and carried the second-largest unattributable probe tail. A
-- harness that signs up through POST /api/auth/register mints a tenant that is shaped exactly like a
-- customer's ("<local-part>-<8hex>" slug, a users row, the free plan on tenant_plans), so every
-- sweep had to guess the origin from the name. From here the origin is recorded AT CREATION: the
-- real signup path reads the request header `X-Swift-Harness` and stores the value verbatim below.
--
-- ADDITIVE, NO BACKFILL. Every existing row keeps probe_harness = NULL, which means "origin
-- unknown" — never "not a probe". Nothing in the app branches on this column; the sweep policy's
-- predicate is `probe_harness IS NOT NULL AND created_at < now() - interval '14 days'`.
--
-- The validation `^[a-z0-9][a-z0-9._-]{2,63}$` after trim + lowercase lives in ONE place, the create
-- path in Rust (src/auth/handlers.rs::harness_marker). A missing header, a non-UTF-8 or
-- non-matching value, or a value taken from a request body/query field all store NULL. No DB CHECK
-- is added on purpose: a second copy of the rule in the schema could drift from the one the create
-- path applies, and a bad value must degrade to NULL, never to a 500 on a public route.

ALTER TABLE tenants ADD COLUMN IF NOT EXISTS probe_harness text;

COMMENT ON COLUMN tenants.probe_harness IS
  'Harness/probe origin of this tenant, stored verbatim from the X-Swift-Harness request header at
   creation. NULL = origin unknown (so: every row that predates migration 099, and every real
   signup that sent no such header). The signup path validates before storing: trim + lowercase +
   ^[a-z0-9][a-z0-9._-]{2,63}$; anything else stores NULL. Read-only marker: no route branches on
   it. See /opt/swift/docs/fleet-probe-residue-policy-2026-09-28.md.';

-- The sweep's own predicate reads only the marked rows; keep that scan off the hot table.
CREATE INDEX IF NOT EXISTS idx_tenants_probe_harness
    ON tenants (created_at) WHERE probe_harness IS NOT NULL;

-- Post-conditions, asserted inside the migration so a silent failure cannot pass as applied.
DO $$
DECLARE n int;
BEGIN
    SELECT count(*) INTO n
    FROM information_schema.columns
    WHERE table_name = 'tenants' AND column_name = 'probe_harness'
      AND data_type = 'text' AND is_nullable = 'YES';
    IF n <> 1 THEN
        RAISE EXCEPTION '099: tenants.probe_harness must exist as nullable text, found %', n;
    END IF;

    SELECT count(*) INTO n
    FROM pg_indexes
    WHERE tablename = 'tenants' AND indexname = 'idx_tenants_probe_harness';
    IF n <> 1 THEN
        RAISE EXCEPTION '099: partial index idx_tenants_probe_harness missing, found %', n;
    END IF;

    SELECT count(*) INTO n FROM tenants WHERE probe_harness IS NOT NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION '099: additive, no backfill — % rows already carry a marker', n;
    END IF;

    RAISE NOTICE '099: tenants.probe_harness added (nullable text, 0 rows marked), partial index created';
END $$;
