-- 144_billing_plan_lines.sql
--
-- THE BILL, AS A FACT (David, 2026-11-11): the Inbox door is STANDALONE. Inbox is $1/mo
-- ($10/yr) and nothing else; upgrading to Starter ($29/mo) CANCELS that $1 in the same action,
-- so a customer is never billed $30 and never holds two subscriptions. Downgrading back to
-- Inbox returns to $1 with the mailbox and every message intact.
--
-- WHY A LEDGER AND NOT JUST `tenant_plans.plan_id`. `tenant_plans` is the ENTITLEMENT row: one
-- per tenant, and it is overwritten in place. That means the row can only ever describe the plan
-- a workspace has NOW — it cannot say that a $1 line existed and ENDED, which is exactly what
-- "the $1 was cancelled" has to mean, and what an operator reads when they stop the recurring
-- charge at the payment provider. So every plan line a workspace has ever carried lives here,
-- with its own price, its own start and its own end.
--
-- THE INVARIANT, ENFORCED BY THE DATABASE: at most ONE OPEN LINE per tenant (partial unique
-- index, `ended_at IS NULL`). Two open lines are literally unrepresentable, so $30 cannot be
-- "stacked" by a future writer, a hand-run UPDATE, or a second payment page — a second open
-- insert raises 23505. That is the guarantee; the trigger below is how the ledger stays true
-- without every caller having to remember.
--
-- THE TRIGGER IS THE ONE WRITER. `tenant_plans` has several writers (signup seat, operator
-- assignment, cancellation, subscription POST/PATCH) and will get more. Making each one also
-- maintain a ledger is how ledgers drift the first time somebody forgets. Instead an AFTER
-- INSERT/UPDATE trigger on `tenant_plans` closes the open line and opens the new one from the
-- row that was actually written — so even a hand SQL correction is recorded. `end_reason` says
-- only WHAT happened ('superseded' / 'canceled'); whether it was an upgrade or a downgrade is
-- derived from the two prices at read time, never guessed from a name.
--
-- NOTHING IS DELETED. A closed line keeps its price, its cycle and its dates. The money history
-- is append-only, like the mail.

CREATE TABLE IF NOT EXISTS plan_lines (
    id             uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id      uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    plan_id        uuid NOT NULL REFERENCES plans(id) ON DELETE RESTRICT,
    plan_slug      varchar(60)   NOT NULL,
    price_monthly  numeric(10,2) NOT NULL,
    price_yearly   numeric(10,2) NOT NULL,
    billing_cycle  varchar(10)   NOT NULL,
    started_at     timestamptz   NOT NULL DEFAULT NOW(),
    ended_at       timestamptz,
    end_reason     varchar(40),
    superseded_by  varchar(60),
    ended_by       uuid,
    created_at     timestamptz   NOT NULL DEFAULT NOW()
);

-- AT MOST ONE OPEN LINE PER TENANT. This is the "$30" guard: a stacked subscription would need
-- two rows with ended_at IS NULL, and this index refuses the second one outright.
CREATE UNIQUE INDEX IF NOT EXISTS plan_lines_one_open_per_tenant
    ON plan_lines (tenant_id) WHERE ended_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_plan_lines_tenant
    ON plan_lines (tenant_id, started_at DESC);

COMMENT ON TABLE plan_lines IS
    'Append-only billing lines: one open line per workspace (partial unique index). A plan change closes the old line and opens the new one in the same statement.';
COMMENT ON COLUMN plan_lines.end_reason IS
    '''superseded'' = a different plan replaced it; ''canceled'' = the subscription ended; ''backfill'' = the line a workspace already carried when this ledger shipped.';

-- ── backfill: every workspace's CURRENT line, so the ledger starts truthful ────────────────────
INSERT INTO plan_lines
    (tenant_id, plan_id, plan_slug, price_monthly, price_yearly, billing_cycle,
     started_at, ended_at, end_reason)
SELECT tp.tenant_id, tp.plan_id, p.slug, p.price_monthly, p.price_yearly, tp.billing_cycle,
       COALESCE(tp.current_period_starts_at, tp.created_at, NOW()),
       CASE WHEN tp.status IN ('canceled', 'expired')
            THEN COALESCE(tp.canceled_at, tp.updated_at, NOW()) END,
       CASE WHEN tp.status IN ('canceled', 'expired') THEN 'backfill' END
  FROM tenant_plans tp
  JOIN plans p ON p.id = tp.plan_id
 WHERE NOT EXISTS (
        SELECT 1 FROM plan_lines l
         WHERE l.tenant_id = tp.tenant_id AND l.ended_at IS NULL);

-- ── the ledger's one writer ────────────────────────────────────────────────────────────────────
CREATE OR REPLACE FUNCTION tenant_plans_sync_open_line() RETURNS trigger AS $fn$
DECLARE
    p_slug text;
    p_month numeric(10,2);
    p_year  numeric(10,2);
BEGIN
    SELECT slug, price_monthly, price_yearly
      INTO p_slug, p_month, p_year
      FROM plans WHERE id = NEW.plan_id;

    -- A plan row that does not resolve cannot mint a line (the FK will refuse the row itself);
    -- never raise from a bookkeeping trigger.
    IF p_slug IS NULL THEN
        RETURN NEW;
    END IF;

    IF TG_OP = 'INSERT' THEN
        INSERT INTO plan_lines
            (tenant_id, plan_id, plan_slug, price_monthly, price_yearly, billing_cycle, started_at)
        VALUES (NEW.tenant_id, NEW.plan_id, p_slug, p_month, p_year, NEW.billing_cycle,
                COALESCE(NEW.current_period_starts_at, NOW()))
        ON CONFLICT DO NOTHING;
        RETURN NEW;
    END IF;

    IF NEW.plan_id IS DISTINCT FROM OLD.plan_id THEN
        -- The old line ENDS before the new one is opened, in this same trigger: one statement,
        -- one open line, never two. `superseded_by` is the plan that took over.
        UPDATE plan_lines
           SET ended_at = NOW(), end_reason = 'superseded', superseded_by = p_slug
         WHERE tenant_id = NEW.tenant_id AND ended_at IS NULL;

        INSERT INTO plan_lines
            (tenant_id, plan_id, plan_slug, price_monthly, price_yearly, billing_cycle, started_at)
        VALUES (NEW.tenant_id, NEW.plan_id, p_slug, p_month, p_year, NEW.billing_cycle,
                COALESCE(NEW.current_period_starts_at, NOW()))
        ON CONFLICT DO NOTHING;

    ELSIF NEW.billing_cycle IS DISTINCT FROM OLD.billing_cycle THEN
        -- Same plan, different cadence: still ONE line, its cycle moves with it.
        UPDATE plan_lines
           SET billing_cycle = NEW.billing_cycle
         WHERE tenant_id = NEW.tenant_id AND ended_at IS NULL;
    END IF;

    IF NEW.status IN ('canceled', 'expired') AND OLD.status NOT IN ('canceled', 'expired') THEN
        UPDATE plan_lines
           SET ended_at = COALESCE(NEW.canceled_at, NOW()), end_reason = 'canceled'
         WHERE tenant_id = NEW.tenant_id AND ended_at IS NULL;
    END IF;

    RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS tenant_plans_sync_open_line ON tenant_plans;
CREATE TRIGGER tenant_plans_sync_open_line
    AFTER INSERT OR UPDATE OF plan_id, billing_cycle, status ON tenant_plans
    FOR EACH ROW EXECUTE FUNCTION tenant_plans_sync_open_line();

-- ── backstop: refuse to finish this migration if any workspace holds two open lines ───────────
DO $do$
DECLARE
    bad integer;
BEGIN
    SELECT count(*) INTO bad FROM (
        SELECT tenant_id FROM plan_lines WHERE ended_at IS NULL
         GROUP BY tenant_id HAVING count(*) > 1) stacked;
    IF bad > 0 THEN
        RAISE EXCEPTION 'plan_lines: % workspace(s) hold more than one open billing line', bad;
    END IF;
END
$do$;
