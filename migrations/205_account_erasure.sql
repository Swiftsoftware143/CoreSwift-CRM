-- 205_account_erasure.sql — a customer can DELETE their own workspace and data (2026-11-24,
-- kanban t_48b65042, fleet standard docs/account-closure-erasure-standard-2026-11-24.md §3-§6).
--
-- `close_account` (migration 153) records a STATE: the rows stay, the operator can reopen. Erasure
-- is the other half and is irreversible: `POST /api/auth/erase-data` destroys the workspace through
-- the ONE writer (`tenant_purge`), which CASCADEs to every `tenants`-referencing table.
--
-- Two things must SURVIVE that cascade, and because every FK to `tenants` is ON DELETE CASCADE a
-- surviving row cannot carry a tenant FK at all. Hence two tables with a plain uuid column and NO
-- foreign key, deliberate and load-bearing:
--
--   * `erasure_billing_retention` — the billing DOCUMENTS tax law requires us to keep (the plan the
--     customer was on and its price history), copied row-for-row before the delete as jsonb. The
--     copy is detached from the workspace NAME on purpose: it carries the erased tenant's uuid and
--     the stored plan/price/period facts, never the company name, slug or an email address.
--   * `account_erasures` — exactly ONE audit row per erasure: who asked (the signed-in user), when,
--     and the counts that were destroyed. The workspace's own `audit_logs` rows go with the
--     cascade; this row is the record that the erasure itself happened, and it never stores the
--     password or the session token (§3.5).
--
-- Idempotent by construction (IF NOT EXISTS), so a re-run is a no-op.

CREATE TABLE IF NOT EXISTS erasure_billing_retention (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The erased workspace's id. NO foreign key: the workspace row is gone by design.
    erased_tenant_id uuid NOT NULL,
    -- 'tenant_plans' (the plan the customer was on) or 'plan_lines' (its price history).
    source           text NOT NULL,
    -- The whole source row as jsonb: plan id, slug, prices, cycle, period, dates.
    document         jsonb NOT NULL,
    retained_at      timestamptz NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS erasure_billing_retention_tenant_idx
    ON erasure_billing_retention (erased_tenant_id);

COMMENT ON TABLE erasure_billing_retention IS
    'Billing documents kept after a customer erases their workspace (tax retention). No FK to tenants: the workspace no longer exists.';

CREATE TABLE IF NOT EXISTS account_erasures (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The erased workspace. NO foreign key, for the same reason as above.
    erased_tenant_id uuid NOT NULL,
    -- Who asked. The user row CASCADEs away with the workspace, so the id is kept here as a value.
    actor_user_id    uuid NOT NULL,
    actor_email      text NOT NULL,
    action           text NOT NULL DEFAULT 'erase_data',
    -- What was destroyed: users/contacts/lists/plan rows, and the billing documents retained.
    records          jsonb NOT NULL DEFAULT '{}'::jsonb,
    erased_at        timestamptz NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS account_erasures_erased_at_idx
    ON account_erasures (erased_at DESC);

COMMENT ON TABLE account_erasures IS
    'One row per successful self-service data erasure (who, when, what was destroyed). Never stores a password or a token.';
