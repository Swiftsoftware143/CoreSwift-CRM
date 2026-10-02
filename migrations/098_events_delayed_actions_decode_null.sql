-- 098: the event bus decodes NULLABLE columns as non-Option, so ONE NULL row loses the whole row
-- for every route over these two tables (kanban t_4f9ba3c1).
--
-- Measured live 2026-10-02 on the deployed binary: inserting a single row with `source IS NULL`
-- makes GET /api/events and GET /api/events/{id} answer 500 with sqlx's own literal
--   error occurred while decoding column "source": unexpected null; try decoding as an `Option`
-- and the same for a delayed_actions row whose `condition_type IS NULL` on GET /api/events/delayed
-- (the shape src/private_email/auto_reply_handler.rs:250 writes). Both tables are `SELECT *` /
-- `RETURNING *` into one FromRow struct each, so ONE NULL-bearing field fails the entire decode.
--
-- The card named the 4 columns the inline-comment scanner fix made visible
-- (events.source/event_type, delayed_actions.condition_type/action_type). They share a struct — and
-- therefore a statement — with 4 more in the same class (events.created_at/processed,
-- delayed_actions.tenant_id/execute_at/created_at), so the whole struct is closed here: a
-- carded-columns-only fix would leave the carded routes 500-ing through a sibling field.
--
-- PER-COLUMN ARM (writer census 2026-10-02, every INSERT site in src/ read):
--   SET NOT NULL : events.source, events.event_type, events.created_at, events.processed
--                  delayed_actions.tenant_id, action_type, execute_at, created_at
--   stays NULLABLE: delayed_actions.condition_type
-- All 5 `INSERT INTO events` sites and 8 of the 9 `INSERT INTO delayed_actions` sites name the
-- column with a non-Option bind or a literal; every site that omits one (events.created_at,
-- events.processed, delayed_actions.created_at) falls back to a non-NULL DEFAULT. No writer outside
-- this crate touches either table (grep of /opt/swift/bin, scripts, n8n: 0 hits) and no migration
-- INSERTs into them.
--
-- The ONE exception is src/private_email/auto_reply_handler.rs:250, which omits condition_type on
-- purpose — migration 073 made that column nullable for exactly that writer and said so. A NULL
-- there is real data ("this delayed action carries no condition"), not an unset field, so it stays
-- NULLABLE and the Rust field becomes Option<String>. SET NOT NULL would 23502 that writer and break
-- the auto-reply queue.
--
-- Guarded: the NULL census runs FIRST and RAISEs naming table.column, so a boot can never
-- half-constrain a table. A failing boot migration exits the process — a crash-loop, not a 500 —
-- so the precondition is checked before the first ALTER, not discovered by it.

DO $$
DECLARE bad text;
BEGIN
    WITH c AS (
        SELECT 'events.source' AS k, count(*) AS n FROM events WHERE source IS NULL
        UNION ALL SELECT 'events.event_type', count(*) FROM events WHERE event_type IS NULL
        UNION ALL SELECT 'events.created_at', count(*) FROM events WHERE created_at IS NULL
        UNION ALL SELECT 'events.processed', count(*) FROM events WHERE processed IS NULL
        UNION ALL SELECT 'delayed_actions.tenant_id', count(*) FROM delayed_actions WHERE tenant_id IS NULL
        UNION ALL SELECT 'delayed_actions.action_type', count(*) FROM delayed_actions WHERE action_type IS NULL
        UNION ALL SELECT 'delayed_actions.execute_at', count(*) FROM delayed_actions WHERE execute_at IS NULL
        UNION ALL SELECT 'delayed_actions.created_at', count(*) FROM delayed_actions WHERE created_at IS NULL
    )
    SELECT string_agg(k || '=' || n::text, ', ' ORDER BY k) INTO bad FROM c WHERE n > 0;

    IF bad IS NOT NULL THEN
        RAISE EXCEPTION '098: refusing to SET NOT NULL — NULL rows present: %', bad;
    END IF;
END $$;

-- The state the decode could not survive becomes unrepresentable. Defaults are untouched, so every
-- existing writer keeps working: an omitted events.event_type still reads 'meeting', an omitted
-- events.processed still reads false, an omitted created_at still reads now().
ALTER TABLE events ALTER COLUMN source SET NOT NULL;
ALTER TABLE events ALTER COLUMN event_type SET NOT NULL;
ALTER TABLE events ALTER COLUMN created_at SET NOT NULL;
ALTER TABLE events ALTER COLUMN processed SET NOT NULL;

ALTER TABLE delayed_actions ALTER COLUMN tenant_id SET NOT NULL;
ALTER TABLE delayed_actions ALTER COLUMN action_type SET NOT NULL;
ALTER TABLE delayed_actions ALTER COLUMN execute_at SET NOT NULL;
ALTER TABLE delayed_actions ALTER COLUMN created_at SET NOT NULL;

-- Post-conditions: the 8 constrained columns must report attnotnull, and the one deliberate NULL
-- must still be allowed. Asserted inside the migration so a silent ALTER failure cannot pass.
DO $$
DECLARE n int;
BEGIN
    SELECT count(*) INTO n
    FROM pg_attribute a JOIN pg_class t ON t.oid = a.attrelid
    WHERE (t.relname, a.attname) IN (
        ('events','source'), ('events','event_type'), ('events','created_at'), ('events','processed'),
        ('delayed_actions','tenant_id'), ('delayed_actions','action_type'),
        ('delayed_actions','execute_at'), ('delayed_actions','created_at'))
      AND a.attnotnull;
    IF n <> 8 THEN
        RAISE EXCEPTION '098: expected 8 NOT NULL columns, found %', n;
    END IF;

    SELECT count(*) INTO n
    FROM pg_attribute a JOIN pg_class t ON t.oid = a.attrelid
    WHERE t.relname = 'delayed_actions' AND a.attname = 'condition_type' AND a.attnotnull;
    IF n <> 0 THEN
        RAISE EXCEPTION '098: delayed_actions.condition_type must stay NULLABLE (auto_reply writer)';
    END IF;

    RAISE NOTICE '098: 8 columns SET NOT NULL; delayed_actions.condition_type left nullable';
END $$;
