-- t_4ca3ecd7 — `limit_api_calls_per_day`: the counter that makes the authored number real.
--
-- t_f49e4299 RETIRED this key because no per-day request counter existed to compare against the
-- ceiling. Measured again here (2026-10-02): the quantity IS countable and the surface IS the app's
-- own API — `personal_api_keys` + `/api/external` (GET /lists, POST /contacts), the endpoint the
-- Integration Centre hands a tenant and the one the sibling apps push captured leads into. What was
-- missing was the counter, not the quantity. The sibling card t_3e3965fc decides the *contacts*
-- ingest contract (capture over the ceiling vs 402 the lead away); this one meters the API CALL
-- FLOW, which the contacts stock cannot meter (a client re-pushing the same lead consumes calls
-- without consuming a contact).
--
-- COUNTER HOME: `api_call_usage`, one row per (tenant_id, UTC day). The day boundary is the PRIMARY
-- KEY, so there is nothing to reset — a new day is a new row. Cost: ONE upsert per authenticated
-- api-key request (that request path already SELECTs the key and UPDATEs `last_used_at`, so this
-- adds one indexed write next to them). Nothing else reads it.
--
-- GUARD: `features::meter_api_call`, called from `external_api::resolve_key` — i.e. only on the
-- api-key surface, never on the console's JWT routes, so a workspace at its daily ceiling keeps
-- working (the wedge class FunnelSwift's t_8ccc8e6a documents). The increment is conditional on
-- being under the ceiling, so a refused request is not counted and the refusal cannot be amplified.
--
-- The authored ladder is restored from `plans.features->>'api_calls_per_day'` below: free 100,
-- starter 1000, pro 1000, professional 10000, enterprise 10000, agency 100000 — non-decreasing, so
-- `scripts/cs-tier-monotonicity.py` stays green, and every active plan carries the key.
--
-- The other key this card inherited, `limit_storage_gb`, stays RETIRED permanently, decided by
-- measurement (docs/ADMIN_GUIDE.md carries the numbers): the schema has no bytea/file/attachment
-- column, there is no upload path, and at each tier's OWN contact ceiling the tenant's whole row
-- footprint is ~0.02% of the GB ceiling it is priced against — so a size source would produce a
-- knob that cannot fire before the contact cap does.

CREATE TABLE IF NOT EXISTS api_call_usage (
    tenant_id  uuid        NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    day        date        NOT NULL,
    calls      integer     NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, day)
);

CREATE INDEX IF NOT EXISTS idx_api_call_usage_day ON api_call_usage (day);

-- Re-register the registry row the admin's Features & Plans panel edits (t_f49e4299 deleted it).
-- `legacy_feature_key` is what keeps the legacy `PATCH {features: {...}}` write path mirrored onto
-- this row (module_registry::sync_legacy_features), and the key/name/unit/sort_order match the row
-- migration 072 seeded, so the matrix the panel renders is unchanged except for this feature.
INSERT INTO module_features
       (module_id, key, name, description, kind, unit, sort_order, is_active, legacy_feature_key)
SELECT m.id, 'limit_api_calls_per_day', 'API calls per day',
       'Authenticated api-key requests a workspace may make per UTC day (GET/POST on /api/external), counted in api_call_usage.',
       'limit', 'calls/day', 70, true, 'api_calls_per_day'
  FROM modules m
 WHERE m.key = 'limits'
ON CONFLICT (key) DO NOTHING;

-- Every plan's ceiling, read once from the plan data (same shape as migration 072 section 5). A plan
-- whose `api_calls_per_day` is absent resolves to enabled = false = ceiling 0 = "not on your plan"
-- on the api-key surface, which is the house rule for every other limit in this module.
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, (s.v IS NOT NULL), s.v
  FROM plans p
  JOIN module_features f ON f.key = 'limit_api_calls_per_day'
 CROSS JOIN LATERAL (SELECT (p.features ->> 'api_calls_per_day')::numeric AS v) s
ON CONFLICT (plan_id, module_feature_id) DO NOTHING;
