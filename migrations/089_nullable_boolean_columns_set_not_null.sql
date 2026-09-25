-- 089_nullable_boolean_columns_set_not_null.sql
--
-- kanban t_8358f61b (chained off t_ab7492c4, same latent class, the other 26 columns).
--
-- The class: a `#[derive(sqlx::FromRow)]` struct field typed `bool` (not `Option<bool>`) whose
-- column is NULLABLE, bound through `SELECT *` / `RETURNING *` / an explicit column list. One NULL
-- in any row fails the WHOLE-ROW decode, so the surface answers 500 (or, worse, falls back to an
-- invented value). `fleet-dbtype-audit.py` cannot see this direction; the hand sweep
-- (/opt/swift/audits/cs-null-tenant-isactive-t_ab7492c4/struct-bool-sweep.py) measured 101
-- (site, field, column) rows / 86 distinct sites / 26 columns / 23 tables on HEAD 92a03d0.
--
-- Arm chosen: make the NULL state UNREPRESENTABLE (SET NOT NULL), not a decode-side `Option<bool>`
-- rewrite of 101 sites. Evidence, measured on live `coreswift_crm` 2026-09-25:
--   * every one of the 26 columns is `boolean` NULLABLE with a non-NULL DEFAULT (true or false);
--   * live NULL count is 0 on all 26 (5 of the tables hold 0 rows at all);
--   * no writer in this app can produce a NULL — all 24 `INSERT INTO <table>` sites that touch
--     these tables OMIT the column entirely (the DEFAULT applies), and the only two that name it
--     bind a non-optional expression (`r.is_active.unwrap_or(true)`,
--     `req.is_default.unwrap_or(false)`); every `UPDATE ... <col> = $n` is either a literal
--     (`= true` / `= false`) or `COALESCE($n, <col>)`, where a NULL bind means KEEP, never SET NULL;
--   * `grep -rn "INTO <table>"` + the bind census is recorded in
--     /opt/swift/audits/cs-nullbool-t_8358f61b/reach-census.txt (per-column, all 26).
-- So the plain `bool` decode is CORRECT once the column cannot be NULL: no JSON shape changes, no
-- consumer churn, and a future writer that ever tries NULL fails LOUDLY at write time (23502) instead
-- of silently 500-ing a read. The codebase's own newer migrations use `BOOLEAN NOT NULL DEFAULT true`
-- (046, 048, 055, 061, 068, 072, 076, ...), so this also normalises drift.
--
-- Pre-flight: the guard below REFUSES to run if any of the 26 columns holds a NULL, naming the
-- column and count, so this migration can never fail halfway through with rows it cannot constrain.
-- Rehearsed as one BEGIN ... ROLLBACK against live before deploy (01-rehearsal.txt).
--
-- t_8358f61b is the deploy marker: `sqlx::migrate!` embeds this file's text in the binary, so the
-- marker string below is greppable in the running executable (see /opt/swift/bin/cs-nullbool-deploy.sh).

DO $$
DECLARE
    r record;
    n bigint;
    bad text := '';
    targets text[][] := ARRAY[
        ARRAY['affiliate_products', 'is_active'],
        ARRAY['affiliates', 'is_active'],
        ARRAY['automation_rules', 'is_active'],
        ARRAY['automation_webhooks', 'is_active'],
        ARRAY['companies', 'is_active'],
        ARRAY['delayed_actions', 'cancelled'],
        ARRAY['delayed_actions', 'executed'],
        ARRAY['events', 'processed'],
        ARRAY['health_thresholds', 'is_active'],
        ARRAY['integrations', 'is_active'],
        ARRAY['list_members', 'added_manually'],
        ARRAY['lists', 'is_active'],
        ARRAY['notification_rules', 'is_active'],
        ARRAY['notifications', 'read'],
        ARRAY['pipeline_stages', 'is_lost'],
        ARRAY['pipeline_stages', 'is_won'],
        ARRAY['pipelines', 'is_active'],
        ARRAY['pipelines', 'is_default'],
        ARRAY['plans', 'is_active'],
        ARRAY['score_rules', 'is_active'],
        ARRAY['tags', 'is_active'],
        ARRAY['telnyx_config', 'is_active'],
        ARRAY['telnyx_numbers', 'is_active'],
        ARRAY['tenants', 'is_active'],
        ARRAY['users', 'is_active'],
        ARRAY['webhook_endpoints', 'is_active']
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
            't_8358f61b: refusing SET NOT NULL - NULL rows exist in: % (clean them up first)', bad;
    END IF;
END $$;

ALTER TABLE affiliate_products  ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE affiliates          ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE automation_rules    ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE automation_webhooks ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE companies           ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE delayed_actions     ALTER COLUMN cancelled       SET NOT NULL;
ALTER TABLE delayed_actions     ALTER COLUMN executed        SET NOT NULL;
ALTER TABLE events              ALTER COLUMN processed       SET NOT NULL;
ALTER TABLE health_thresholds   ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE integrations        ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE list_members        ALTER COLUMN added_manually  SET NOT NULL;
ALTER TABLE lists               ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE notification_rules  ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE notifications       ALTER COLUMN read            SET NOT NULL;
ALTER TABLE pipeline_stages     ALTER COLUMN is_lost         SET NOT NULL;
ALTER TABLE pipeline_stages     ALTER COLUMN is_won          SET NOT NULL;
ALTER TABLE pipelines           ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE pipelines           ALTER COLUMN is_default      SET NOT NULL;
ALTER TABLE plans               ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE score_rules         ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE tags                ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE telnyx_config       ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE telnyx_numbers      ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE tenants             ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE users               ALTER COLUMN is_active       SET NOT NULL;
ALTER TABLE webhook_endpoints   ALTER COLUMN is_active       SET NOT NULL;
