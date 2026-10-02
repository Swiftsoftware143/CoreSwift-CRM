-- 106_opportunities_won_lost_set_not_null.sql
--
-- kanban t_e5405fb1 (found by fleet-dbtype-audit.py after t_1704f476 unblinded the
-- `concat!`-assembled SQL in src/pipelines/opportunity.rs: the `opp_cols!()` macro is now read with
-- its real item list, so the `OpportunityFull` decode sites became visible for the first time).
--
-- The class: `OpportunityFull` (src/pipelines/opportunity.rs:45) decodes `is_won: bool` and
-- `is_lost: bool` (not `Option<bool>`), through the shared `opp_cols!()` item list, at 3 sites
-- (list / get / move_opportunity). One NULL in either column fails the WHOLE-ROW decode, so the
-- pipeline board, the deal detail and the board's move action would all answer 500.
--
-- This is the SAME class migration 089_nullable_boolean_columns_set_not_null.sql closed for 26
-- columns (t_8358f61b) - and `opportunities.is_won/is_lost` were simply MISSED there:
-- 089 constrained `pipeline_stages.is_won/is_lost` but not their `opportunities` twins, which the
-- baseline ships as `boolean DEFAULT false` WITHOUT the NOT NULL (migrations/000:1814-1815).
--
-- Arm chosen: make the NULL state UNREPRESENTABLE (SET NOT NULL), not a decode-side `Option<bool>`
-- rewrite. Evidence, measured live 2026-10-02 on `coreswift_crm` (audits/t_e5405fb1/):
--   * both columns are `boolean` NULLABLE with a non-NULL DEFAULT `false`; live rows = 2, live
--     NULLs = 0 (0 in either column);
--   * no writer can produce a NULL. The only `INSERT INTO opportunities` in the crate
--     (src/pipelines/opportunity.rs:216) OMITS both columns, so the DEFAULT `false` applies - proved
--     by running the app's own INSERT shape against a copy of the live database and reading the row
--     back (is_won IS NULL = f, is_lost IS NULL = f). Every `UPDATE opportunities ... is_won/is_lost`
--     is either `COALESCE($n, <col>)` where a NULL bind means KEEP (opportunity.rs:285-286) or a
--     non-optional `bool` bind (pipelines/handlers.rs:296). `automation/actions.rs:107` and
--     `scoring/engine.rs:136` only move `stage_id`.
-- So the plain `bool` decode is CORRECT once the column cannot be NULL: no JSON shape changes, no
-- consumer churn, and a future writer that ever tries a NULL fails LOUDLY at write time (23502)
-- instead of silently 500-ing a read.
--
-- Pre-flight: the guard below REFUSES to run if either column holds a NULL, naming the column and
-- count, so this migration can never fail halfway through with rows it cannot constrain.
-- Rehearsed as one BEGIN ... ROLLBACK against live before deploy, and on a throwaway copy of the
-- live database with a committed NULL row (both ways: guard fires with the NULL present; the NULL
-- write is rejected 23502 afterwards).

DO $$
DECLARE
    r record;
    n bigint;
    bad text := '';
    targets text[][] := ARRAY[
        ARRAY['opportunities', 'is_won'],
        ARRAY['opportunities', 'is_lost']
    ];
BEGIN
    FOR i IN 1 .. array_length(targets, 1) LOOP
        EXECUTE format('SELECT count(*) FROM %I WHERE %I IS NULL', targets[i][1], targets[i][2])
            INTO n;
        IF n > 0 THEN
            bad := bad || format('%s.%s=%s ', targets[i][1], targets[i][2], n);
        END IF;
    END LOOP;
    IF bad <> '' THEN
        RAISE EXCEPTION
            't_e5405fb1: refusing SET NOT NULL - NULL rows exist in: % (backfill them first)', bad;
    END IF;
END $$;

ALTER TABLE opportunities ALTER COLUMN is_won  SET NOT NULL;
ALTER TABLE opportunities ALTER COLUMN is_lost SET NOT NULL;
