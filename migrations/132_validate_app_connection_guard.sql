-- t_a1ada5ef (second half of the guard): make `migrations/` alone reproduce the VALIDATED
-- constraint that the LIVE database ends up with.
--
-- `131` arms the CHECK as NOT VALID so a table that still holds legacy plaintext rows can be
-- guarded without a rewrite; the boot half (`native_apps::credentials::validate_app_connection_guard`
-- -> backfill, then VALIDATE) makes it fully enforced afterwards. `pg_get_constraintdef` prints
-- `NOT VALID` for an unvalidated constraint, so without this file the from-zero build (which runs
-- migrations only, no boot code) would carry the NOT VALID form while LIVE carries the validated
-- one - a permanent CATALOG DRIFT for the nightly from-zero watch (measured on this card's own
-- second deploy: `CLASS coreswift constraints missing=1 extra=1`).
--
-- Guarded on purpose: on a fresh install `app_connections` is empty and this validates immediately,
-- which is exactly the shape LIVE converges to. On a database that still holds legacy plaintext
-- rows the block leaves the constraint NOT VALID and the boot half seals the rows first - a plain
-- VALIDATE here would ABORT the boot on such a database.
--
-- No semicolon anywhere in these comments - the in-app runner executes the whole file.
DO $app_conn_guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM app_connections
         WHERE credentials->>'api_key' IS NOT NULL
           AND credentials->>'api_key' <> ''
           AND credentials->>'api_key' NOT LIKE 'enc:v1:%'
    ) THEN
        ALTER TABLE app_connections VALIDATE CONSTRAINT app_connections_credentials_sealed;
    END IF;
END
$app_conn_guard$;
