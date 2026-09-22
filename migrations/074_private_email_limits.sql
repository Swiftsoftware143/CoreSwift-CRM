-- CS-20 — per-plan private-email limits were configured NOWHERE.
--
-- `tenant_email_limits` had 0 rows and no plan defined `max_domains` / `max_mailboxes`, so
-- `check_domain_limit` / `check_mailbox_limit` read 0 for every tenant and the sold limits
-- (Starter 1 domain / 3 mailboxes, Pro 3 / 10, Enterprise unlimited / 50) were enforced by nothing.
--
-- The limits are DATA, not code: they live on the module-registry assignment rows that the admin
-- panel edits (`module_features.kind='limit'` for the `private_email` module, assigned per plan in
-- `plan_module_features`). `plans.features` is NOT touched here — the 6 plan rows stay byte-identical.
--
-- Semantics, matching the resolver (`module_registry::resolve`):
--   enabled = false            -> the plan does not get the feature at all (free, for this module)
--   enabled = true, NULL limit -> UNLIMITED
--   enabled = true, N (> 0)    -> capped at N;  0 -> "not available on your plan"
--
-- Only rows whose limit is still unset are written, so a number an admin has since changed in the
-- console is never overwritten if this file is ever replayed by hand.
--
-- professional / agency are not named in the sold table. professional mirrors `pro` (it is the same
-- price point one step up), agency mirrors `enterprise` (top tier, unlimited domains / 50 mailboxes).
-- These two are flagged to David; both are editable in the admin panel with no deploy.

UPDATE plan_module_features pf
   SET limit_value = w.limit_value,
       enabled     = true,
       updated_at  = now()
  FROM (VALUES
        ('email_domains',   'free',         0::float8),
        ('email_domains',   'starter',      1::float8),
        ('email_domains',   'pro',          3::float8),
        ('email_domains',   'professional', 3::float8),
        ('email_domains',   'enterprise',   NULL::float8),
        ('email_domains',   'agency',       NULL::float8),
        ('email_mailboxes', 'free',         0::float8),
        ('email_mailboxes', 'starter',      3::float8),
        ('email_mailboxes', 'pro',          10::float8),
        ('email_mailboxes', 'professional', 10::float8),
        ('email_mailboxes', 'enterprise',   50::float8),
        ('email_mailboxes', 'agency',       50::float8)
       ) AS w(feature_key, plan_slug, limit_value)
  JOIN module_features f ON f.key = w.feature_key
  JOIN plans p           ON p.slug = w.plan_slug
 WHERE pf.plan_id = p.id
   AND pf.module_feature_id = f.id
   AND pf.limit_value IS NULL;
