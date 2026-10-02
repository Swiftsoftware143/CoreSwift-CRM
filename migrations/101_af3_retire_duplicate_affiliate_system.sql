-- 101_af3_retire_duplicate_affiliate_system.sql
-- kanban t_3d81b041 (AF-3) — David's model: the affiliate system lives ONLY in FunnelSwift, and a
-- sibling app CONNECTS to it. CoreSwift carried a full DUPLICATE system (affiliate profiles,
-- referrals, commission payouts, a product board and its commission maths). This card retired it:
-- src/affiliates/ and the `/api/affiliates` nest are gone, the two chat intents that created
-- CRM-side affiliate profiles/products are gone, the hub actions that exposed the family to
-- integrator webhook tokens are gone, and both served consoles lost the Affiliates tab that called
-- those routes.
--
-- Two pieces of DB state pointed at that retired surface and are repaired here.
--
-- (1) `automation_webhooks.allowed_actions` is a SERVED CONTRACT: it is echoed back to integrators
--     (webhooks.list) and the public handler refuses anything NOT in it. The trigger that
--     auto-creates a token for every new tenant granted 7 affiliate actions, and 14 live rows still
--     advertised them (measured 2026-10-01). Leaving them would re-introduce exactly the defect
--     migration 094 closed: a token advertising an action route_action() no longer implements,
--     answering 400 "Unknown action" to every caller. The trigger loses the 7 and every existing row
--     is back-filled; array_remove is a no-op when the element is absent, so the back-fill is
--     idempotent.
--
-- (2) The `affiliates` module/feature is the plan entitlement whose FeatureGate wrapped the retired
--     router. With the router gone that gate had no consumer left, so leaving it enabled would have
--     turned it into a switch the admin panel sells and no code reads (the t_f49e4299 class, carded
--     for 7 limit keys in the same change series). It is retired the way the registry itself
--     expresses "not sold": modules.is_active = false — which the served user guide's catalogue
--     query (scripts/cs-guides-build.py) and scripts/cs-tier-monotonicity.py both key off — plus
--     every plan assignment disabled, so the top tier's "grants every module" invariant still holds.
--     The ROWS ARE RETAINED: nothing is deleted, so the decision stays reversible from the panel.
--
-- NOT dropped, on purpose, and this is the card's reader census talking: the tables themselves
-- (`affiliates`, `referrals`, `commission_payouts`, `affiliate_products`,
-- `affiliate_product_selections`). They hold 0 rows (measured 2026-10-01) and
-- `affiliates.user_id -> users.id` is the fleet's identity anchor for an affiliate account — the
-- card says keep the link until a census proves nothing reads it, and after this change the census
-- is exactly zero readers, i.e. an empty, harmless schema rather than a load-bearing one. Dropping
-- is a separate, reversible-at-cost decision that no requirement here forces.

CREATE OR REPLACE FUNCTION auto_create_webhook()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO automation_webhooks (id, tenant_id, name, allowed_actions)
    VALUES (
        gen_random_uuid(),
        NEW.id,
        'Auto-generated for ' || NEW.name,
        ARRAY['contacts.list', 'contacts.create', 'contacts.get', 'contacts.update',
              'tags.list', 'tags.assign', 'tags.unassign',
              'lists.list', 'lists.members',
              'pipelines.list', 'pipelines.opportunities', 'pipelines.stages', 'pipelines.create_stage',
              'tenants.create', 'tenants.settings',
              'users.invite', 'users.list',
              'webhooks.generate', 'webhooks.revoke', 'webhooks.list',
              'scoring.calculate',
              'analytics.contacts',
              'audit.log',
              'search.query',
              'comms.send', 'comms.templates',
              'automation.list',
              'events.ingest',
              'native.connect', 'native.sync.push', 'native.sync.pull',
              'billing.plans', 'billing.credits',
              'ai.assess', 'ai.compose', 'ai.recommend',
              'directory.listings', 'directory.listings.create', 'directory.listings.get',
              'directory.listings.update', 'directory.reviews', 'directory.followups',
              'directory.analytics', 'directory.health']
    );
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Back-fill every row that still advertises a retired affiliate action (14 rows measured).
UPDATE automation_webhooks
   SET allowed_actions = array_remove(
                             array_remove(
                               array_remove(
                                 array_remove(
                                   array_remove(
                                     array_remove(
                                       array_remove(allowed_actions, 'affiliates.profile'),
                                       'affiliates.referrals'),
                                     'affiliates.stats'),
                                   'affiliate_products.list'),
                                 'affiliate_products.my'),
                               'affiliate_products.select'),
                             'affiliate_products.unselect'),
       updated_at = NOW()
 WHERE allowed_actions && ARRAY['affiliates.profile', 'affiliates.referrals', 'affiliates.stats',
                                'affiliate_products.list', 'affiliate_products.my',
                                'affiliate_products.select', 'affiliate_products.unselect'];

-- Retire the plan entitlement (rows retained; the registry's own "not sold" marker).
UPDATE plan_module_features
   SET enabled = false, updated_at = NOW()
 WHERE enabled
   AND module_feature_id IN (SELECT id FROM module_features WHERE key = 'affiliates');

UPDATE plan_modules
   SET enabled = false, updated_at = NOW()
 WHERE enabled
   AND module_id IN (SELECT id FROM modules WHERE key = 'affiliates');

UPDATE module_features
   SET is_active = false, updated_at = NOW()
 WHERE is_active AND key = 'affiliates';

UPDATE modules
   SET is_active = false, updated_at = NOW()
 WHERE is_active AND key = 'affiliates';
