-- AF-5 (kanban t_6d09f02c): "The top tier plan gets everything."
--
-- David's directive (2026-09-23): every app's TOP tier grants EVERY feature its own registry
-- defines. The registry is `modules` / `module_features`; the assignment is `plan_modules` /
-- `plan_module_features`. Two repairs, both idempotent, both GAP-FILLING only.
--
-- (a) TOP-TIER SUPERSET.
--     TYPE: data / entitlement (no DDL).
--     TOP is derived from live data, never assumed: the active plan with the highest
--     `price_monthly`, ties broken by `sort_order` then slug. Measured 2026-10-01 that is
--     `agency` (sort_order 4, $149/mo) — and it already grants 22/22 modules and 32/32 features,
--     so this arm is a NO-OP today. It exists so that a module or feature added to the
--     catalogue later cannot silently leave the top tier behind: the next boot fills the gap.
--     `enabled` is what is set, because the directive is about WHICH KEYS the top tier carries.
--     `limit_value` is NEVER touched — a ceiling the owner set stays exactly as authored.
--
-- (b) THE INVERTED TIER (same card, same data).
--     A higher tier carrying FEWER keys than a cheaper one is a data bug. Measured:
--     `professional` ($79, sort_order 2) had `limit_max_widgets` DISABLED while `pro` ($29,
--     sort_order 2) had it on at 2, and while `enterprise` ($79, sort_order 3) had it on at 7 —
--     so upgrading pro -> professional would have LOST widgets. This is the only inversion left
--     in the ladder: every other key already satisfies "no cheaper plan beats a more expensive
--     one" (checked by scripts/cs-tier-monotonicity.py).
--     Repaired to 5: above the cheaper sibling (pro = 2) and below the next tier up
--     (enterprise = 7), which is the same shape `professional` already uses for
--     limit_integrations (5) and limit_pipelines (5).
--     GUARDED on the exact seeded state (`enabled = false AND limit_value IS NULL`) so a re-run
--     cannot fight an assignment the admin has since made from the Features & Plans panel: once
--     the owner has touched that cell, this statement no longer matches it.
--
-- NOT touched: every other tier's entitlements, every `plans` column (prices, names,
-- `plans.features`), and every `limit_value` on the top tier.

DO $$
DECLARE
    top_id uuid;
    filled_modules int;
    filled_features int;
    repaired int;
BEGIN
    -- (a) the top tier, established from live data
    SELECT id INTO top_id
      FROM plans
     WHERE is_active
     ORDER BY price_monthly DESC, COALESCE(sort_order, 0) DESC, slug
     LIMIT 1;

    IF top_id IS NOT NULL THEN
        INSERT INTO plan_modules (plan_id, module_id, enabled)
        SELECT top_id, m.id, true
          FROM modules m
         WHERE m.is_active
        ON CONFLICT (plan_id, module_id)
        DO UPDATE SET enabled = true, updated_at = now()
        WHERE plan_modules.enabled IS DISTINCT FROM true;
        GET DIAGNOSTICS filled_modules = ROW_COUNT;

        INSERT INTO plan_module_features (plan_id, module_feature_id, enabled)
        SELECT top_id, f.id, true
          FROM module_features f
         WHERE f.is_active
        ON CONFLICT (plan_id, module_feature_id)
        DO UPDATE SET enabled = true, updated_at = now()
        WHERE plan_module_features.enabled IS DISTINCT FROM true;
        GET DIAGNOSTICS filled_features = ROW_COUNT;

        RAISE NOTICE 'AF-5(a): top plan % -> % module grants, % feature grants filled',
                     top_id, filled_modules, filled_features;
    END IF;

    -- (b) the one inverted row (see header)
    UPDATE plan_module_features pf
       SET enabled = true, limit_value = 5, updated_at = now()
      FROM plans p, module_features f
     WHERE pf.plan_id = p.id
       AND pf.module_feature_id = f.id
       AND p.slug = 'professional'
       AND f.key = 'limit_max_widgets'
       AND pf.enabled = false
       AND pf.limit_value IS NULL;
    GET DIAGNOSTICS repaired = ROW_COUNT;
    RAISE NOTICE 'AF-5(b): professional limit_max_widgets repaired rows=%', repaired;
END $$;
