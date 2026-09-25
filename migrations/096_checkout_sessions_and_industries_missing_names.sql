-- 096_checkout_sessions_and_industries_missing_names.sql
-- CoreSwift-CRM: three statements named relations this database never had (card t_a8a3fa27,
-- found by the TABLE-MISSING class of fleet-dbtype-audit.py).  One verdict per site, from the
-- decode side; evidence in /opt/swift/audits/t_a8a3fa27/.
--
--   checkout_sessions  ->  CREATE TABLE (this file).  src/billing/handlers.rs has THREE statements
--       against it: the INSERT at :701 (POST /api/billing/checkout/create), and the webhook
--       UPDATEs at :796/:873 ("status = 'completed' ... WHERE provider_session_id = $1 AND
--       status = 'pending' RETURNING tenant_id, metadata").  REPOINT is impossible: no relation in
--       coreswift_crm carries `provider_session_id` (checked in pg_class/information_schema for the
--       whole database), and the pending->completed state machine plus the metadata column are what
--       the webhooks need to deliver what was bought.  The code shipped in commit 439590f with no
--       migration ever creating the table, so the INSERT was a 42P01 from the first day: live
--       POST /api/billing/checkout/create returned 500 "Database error" (proven, see REPORT).
--
--   industries         ->  CREATE VIEW (this file) over `user_industry_dashboards`, the only
--       tenant-scoped industry model this app actually has.  The name was PORTED IN from a sibling
--       app: the statement is `SELECT COUNT(*) FROM industries WHERE tenant_id = $1`
--       (src/features.rs:60 in enforce_feature_limit and :170 in count_usage, both reached through
--       get_usage_json -> GET /api/auth/me/usage), while incentiveswift's `industries` is a global
--       catalogue with NO tenant_id, so it could not have answered that statement either.  Both
--       statements are untouched.  A lossless single-table view (no aggregate, no join) also keeps
--       the ported name WRITABLE: Postgres auto-updates through it into user_industry_dashboards.
--       NOTE, deliberately stated: the underlying table has no live writer yet (the `industries`
--       module is declared in src/main.rs:37 but its router is never nested - a separate card).
--       That is exactly why a VIEW is the right shape here and a base table would not be: a table
--       with no writer has a structurally 0 COUNT(*), i.e. a gate that stops erroring and starts
--       lying.  A mirror view cannot drift from the model it mirrors.
--
--   outbound_sms       ->  NO DDL, and none is wanted.  src/telnyx/handlers.rs:390-400 already
--       writes the same event to `outbound_messages` (channel='sms') one statement earlier; the
--       phantom INSERT only re-recorded it with from_number + telnyx_message_id, and nothing in
--       the repository ever READS outbound_sms.  `outbound_messages.message_id` already exists and
--       is the provider-message-id column, so the tracking id moves there (Rust change) and the
--       name disappears instead of being invented.  A second table with no reader is the worse
--       defect (see the t_d3e0bab6 drop decision for the same reasoning on 13 tables).
--
-- Both objects are idempotent so this file converges on a fresh, restored or already-fixed
-- database.  No BEGIN/COMMIT (the runner wraps each migration).

CREATE TABLE IF NOT EXISTS checkout_sessions (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id           uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id             uuid REFERENCES users(id) ON DELETE SET NULL,
    provider_type       text NOT NULL,
    provider_session_id text NOT NULL,
    purchasable_type    text NOT NULL,
    purchasable_id      uuid,
    amount              numeric(10,2) NOT NULL DEFAULT 0,
    currency            text NOT NULL DEFAULT 'USD',
    metadata            jsonb NOT NULL DEFAULT '{}'::jsonb,
    return_url          text NOT NULL DEFAULT '',
    status              text NOT NULL DEFAULT 'pending',
    webhook_received_at timestamptz,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);

-- The webhook statements look a session up by provider_session_id and filter on status='pending'.
CREATE INDEX IF NOT EXISTS idx_checkout_sessions_provider_session
    ON checkout_sessions (provider_session_id, status);

CREATE INDEX IF NOT EXISTS idx_checkout_sessions_tenant
    ON checkout_sessions (tenant_id, created_at DESC);

COMMENT ON TABLE checkout_sessions IS
    'Provider checkout sessions (card t_a8a3fa27): written by POST /api/billing/checkout/create, '
    'marked completed by the provider webhooks.  Persistence only - credit delivery is recorded in '
    'credit_transactions.';

CREATE OR REPLACE VIEW industries AS
    SELECT id,
           tenant_id,
           user_id,
           industry_slug,
           dashboard_name,
           is_active,
           created_at,
           updated_at
      FROM user_industry_dashboards;

COMMENT ON VIEW industries IS
    'Ported-in name for the tenant-scoped industry model (card t_a8a3fa27).  Lossless mirror of '
    'user_industry_dashboards, so COUNT(*) FROM industries WHERE tenant_id = $1 is a real number '
    'and a writer through this name lands in user_industry_dashboards.  The base table is the '
    'target for DDL and for anything that needs explicit column semantics.';
