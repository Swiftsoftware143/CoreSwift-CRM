-- 126_inbox_plan.sql
-- CoreSwift "Inbox" plan: a standalone $1/mo door with one mailbox and NOTHING else.
-- Brief: /opt/swift/docs/coreswift-inbox-plan-brief.md (card t_2ef67217).
--
-- Also settles the measured inconsistency: private_email is ENABLED on the Free plan but
-- has no email_mailboxes limit row, so the gate resolves the limit to zero and the feature
-- is silently unavailable instead of being refused with a message. Free now carries a
-- DEFINED limit (0) so "module on, limit missing" can never happen again.
--
-- is_active is deliberately FALSE: the brief requires the payment provider to be confirmed
-- live before the plan is sellable. Flip it in one UPDATE once checkout is proven, so no
-- customer ever reaches a dead button.

-- 1. make room directly after Free
UPDATE plans SET sort_order = sort_order + 1
 WHERE is_active AND sort_order >= 1;

-- 2. the plan itself (idempotent)
INSERT INTO plans (name, slug, description, price_monthly, price_yearly, is_active, sort_order)
SELECT 'Inbox', 'inbox',
       'Private inbox only - one mailbox on your own domain.',
       1.00, 10.00, false, 1
 WHERE NOT EXISTS (SELECT 1 FROM plans WHERE slug = 'inbox');

-- 3. module gate: private_email ON, every other module OFF
--    (the inbox plan must grant no CRM capability)
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, (m.key = 'private_email')
  FROM plans p
  JOIN modules m ON true
 WHERE p.slug = 'inbox'
   AND NOT EXISTS (
        SELECT 1 FROM plan_modules pm
         WHERE pm.plan_id = p.id AND pm.module_id = m.id);

-- 4. private_email feature limits for the inbox plan
--    one mailbox, one domain, unlimited history (limit_value NULL = no ceiling)
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, mf.id, true,
       CASE mf.key
            WHEN 'email_mailboxes'    THEN 1
            WHEN 'email_domains'      THEN 1
            WHEN 'email_history_days' THEN NULL
            ELSE NULL
       END
  FROM plans p
  JOIN module_features mf ON true
  JOIN modules m ON m.id = mf.module_id
 WHERE p.slug = 'inbox'
   AND m.key = 'private_email'
   AND NOT EXISTS (
        SELECT 1 FROM plan_module_features x
         WHERE x.plan_id = p.id AND x.module_feature_id = mf.id);

-- 5. Free keeps the module but now with a DEFINED mailbox limit of 0
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, mf.id, true, 0
  FROM plans p
  JOIN module_features mf ON true
  JOIN modules m ON m.id = mf.module_id
 WHERE p.slug = 'free'
   AND m.key = 'private_email'
   AND mf.key = 'email_mailboxes'
   AND NOT EXISTS (
        SELECT 1 FROM plan_module_features x
         WHERE x.plan_id = p.id AND x.module_feature_id = mf.id);
