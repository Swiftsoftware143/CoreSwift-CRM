-- The creation hole, closed where it actually is: a portfolio company inserted by SQL, a migration or a script
-- must still provision every app. Refusing the insert would only move the silent failure somewhere else (CEO,
-- 2026-11-15), so the database itself enqueues the work.
--
-- The trigger fires on INSERT and on a change to any of the fields a sibling cares about, and NOT on
-- account_tenant_id — that column is written by provisioning itself, and counting it would make every sync
-- enqueue the next one for ever.
--
-- The worker drains this table on the same tick that already watches the response clocks, provisions locally
-- (tenant, owner, one key, welcome mail) and then broadcasts per target with a log line each.

CREATE TABLE IF NOT EXISTS portfolio_sync_outbox (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    portfolio_id  uuid NOT NULL REFERENCES portfolio_companies(id) ON DELETE CASCADE,
    action        text NOT NULL DEFAULT 'upsert',
    attempts      integer NOT NULL DEFAULT 0,
    last_error    text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    processed_at  timestamptz
);

CREATE INDEX IF NOT EXISTS idx_portfolio_sync_outbox_pending
    ON portfolio_sync_outbox (created_at)
    WHERE processed_at IS NULL;

-- Which account a company owns here, so a re-sync tops up the SAME account instead of guessing by email.
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS account_tenant_id uuid;

CREATE OR REPLACE FUNCTION portfolio_sync_enqueue() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO portfolio_sync_outbox (portfolio_id, action) VALUES (NEW.id, 'create');
    ELSIF NEW.name        IS DISTINCT FROM OLD.name
       OR NEW.slug         IS DISTINCT FROM OLD.slug
       OR NEW.email        IS DISTINCT FROM OLD.email
       OR NEW.description  IS DISTINCT FROM OLD.description
       OR NEW.is_active    IS DISTINCT FROM OLD.is_active THEN
        INSERT INTO portfolio_sync_outbox (portfolio_id, action) VALUES (NEW.id, 'update');
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_portfolio_sync_enqueue ON portfolio_companies;
CREATE TRIGGER trg_portfolio_sync_enqueue
    AFTER INSERT OR UPDATE ON portfolio_companies
    FOR EACH ROW EXECUTE FUNCTION portfolio_sync_enqueue();

-- Backfill: every company that exists now gets exactly one pending pass, so the one that was created by hand
-- (BV Premiere Products) is provisioned and broadcast like any other, and the five already-live ones re-assert
-- their shape without duplicating anything.
INSERT INTO portfolio_sync_outbox (portfolio_id, action)
SELECT c.id, 'update' FROM portfolio_companies c
 WHERE NOT EXISTS (SELECT 1 FROM portfolio_sync_outbox o WHERE o.portfolio_id = c.id);
