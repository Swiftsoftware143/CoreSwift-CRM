-- t_f49e4299 — the seven registry limit features that were stored, panel-editable and read by
-- NOTHING: wire or retire each. Five of the eleven `module_features.kind='limit'` rows were already
-- enforced (`limit_max_widgets`, `limit_max_industries`, `email_domains`, `email_mailboxes`, plus the
-- industries one), so this migration only settles the seven.
--
-- WIRED in the same change (`src/features.rs::enforce_usage_limit`, called ONLY on the route that
-- ADDS the counted row — a usage ceiling copied onto a GET/PATCH/DELETE wedges the tenant at its
-- limit, see FunnelSwift t_8ccc8e6a):
--   limit_max_contacts  -> POST /api/contacts, POST /api/csv/import/contacts,
--                          POST /api/external/*/contacts (new-row arm only)
--   limit_max_users     -> POST /api/auth/register (invite acceptance is the seat-add path)
--   limit_pipelines     -> POST /api/pipelines
--   limit_integrations  -> POST /api/integrations
--
-- RETIRED HERE. A number the admin can type must not be a lie, and each of these three has no
-- measurable quantity behind it. They are DELETED rather than deactivated because the console marks
-- an `is_active = false` row "(inactive)" and still renders its number box:
--   * limit_storage_gb        — no size source exists anywhere in the app: `account_health.storage_mb`
--                               is declared and never written by any code path. Retired, not guessed.
--   * limit_api_calls_per_day — no request counter exists (no per-day usage table, no write on the
--                               request path). Building the counter is a feature of its own.
--   * limit_monthly_credits   — a DUPLICATE of `plans.monthly_credits`, which is what the credit
--                               engine actually reads (`src/billing/credits.rs`, two readers, plus
--                               `credit_transactions` / billing-period machinery). Migration 072
--                               SEEDED this registry row from that column; the row was read by
--                               nothing. The column wins; the copy goes.
--
-- The authored numbers are NOT lost: `plans.features` still carries `storage_gb` /
-- `api_calls_per_day`, and `plans.monthly_credits` keeps the credits allowance. Nothing in this
-- file touches plan data.

DELETE FROM plan_module_features
 WHERE module_feature_id IN (
     SELECT id FROM module_features
      WHERE key IN ('limit_storage_gb', 'limit_api_calls_per_day', 'limit_monthly_credits'));

DELETE FROM module_features
 WHERE key IN ('limit_storage_gb', 'limit_api_calls_per_day', 'limit_monthly_credits');

-- `plans.max_contacts` is the same defect from the other side: migration 072 seeded
-- `limit_max_contacts` from `plans.features->>'max_contacts'` (the authored ladder 100 … 50000),
-- while this column sat at a uniform 1000 on every plan and was read by NO code (grep: only a
-- comment). Two sources of truth for one quota is the defect, and the dead one is the version the
-- next reader wires by mistake. The registry row wins: it is what the console edits and what the
-- guard reads. The JSONB key keeps its numbers.
ALTER TABLE plans DROP COLUMN IF EXISTS max_contacts;
