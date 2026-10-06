-- CoreSwift: the ACCOUNT MATCHING module (migration 113).
--
-- The seventh back-end specialist had no code and no registry row: matching an inbound identity to
-- the contacts/companies a tenant already owns was written out inline (and differently) in four
-- places. `src/account_match/` now owns it (engine + a two-route HTTP surface behind the module's own
-- plan gate). This migration is the DATA half — the module's registry row and its own plan tier —
-- and it is ADDITIVE, exactly like 072 was:
--
--   * no `plans` row, column, price, name or `plans.features` key is touched
--   * every statement is idempotent (ON CONFLICT DO NOTHING), so a re-run cannot clobber an
--     assignment the admin has since made by hand
--   * `plan_modules` / `plan_module_features` are the tables the admin's Features & Plans panel
--     writes, so the tier seeded here is changed with one press, not a deploy
--
-- TIER (the one judgement this file makes, and it is reversible in one press): account matching is
-- NOT part of `free`; it is granted from `starter` up. That mirrors the two modules that already
-- carry a tier distinction of exactly this shape — Support tickets and the Private mailbox (072
-- section 3 + the private_email UPDATE below it) — and it is the same customer-facing shape as an
-- advanced pipeline-integrity capability: the capture paths keep working on every plan (they are
-- gated by their own ceilings), and it is the matching SERVICE that is sold. `free=false` cannot
-- regress anybody: the module's HTTP surface is new in this release, so no shipped client calls it.

INSERT INTO modules (key, name, description, icon, sort_order, legacy_feature_key)
VALUES (
    'account_match',
    'Account matching',
    'Match inbound identities to the contacts and companies this workspace already has',
    '🎯',
    230,
    NULL
)
ON CONFLICT (key) DO NOTHING;

-- Every other module carries a boolean feature named after itself (072 section 2); this one does too,
-- so the admin matrix can fine-tune the module and its feature independently.
INSERT INTO module_features (module_id, key, name, kind, unit, sort_order, legacy_feature_key)
SELECT m.id, m.key, m.name, 'boolean', NULL, 0, NULL
  FROM modules m
 WHERE m.key = 'account_match'
ON CONFLICT (key) DO NOTHING;

-- The module's own plan tier: one assignment row per plan, `free` excluded.
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, (p.slug <> 'free')
  FROM plans p
  CROSS JOIN modules m
 WHERE m.key = 'account_match'
ON CONFLICT (plan_id, module_id) DO NOTHING;

-- ...and the matching per-feature row, mirroring the module assignment (072 section 4).
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT pm.plan_id, f.id, pm.enabled, NULL
  FROM plan_modules pm
  JOIN modules m ON m.id = pm.module_id
  JOIN module_features f ON f.module_id = m.id AND f.key = m.key
 WHERE m.key = 'account_match'
ON CONFLICT (plan_id, module_feature_id) DO NOTHING;
