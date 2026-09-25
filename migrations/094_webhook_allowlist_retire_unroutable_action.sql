-- 094_webhook_allowlist_retire_unroutable_action.sql
-- t_e041e281 — the automation_webhooks allow-list is a SERVED CONTRACT: it is echoed back to
-- integrators (webhooks.list / the GET webhook list) and the public handler refuses anything not
-- in it (403 "Action 'x' not allowed for this webhook"). Six actions the trigger granted had no
-- arm in route_action() (src/webhook/actions.rs), measured live 2026-09-25: each answered
-- 400 {"error":"Unknown action: <action>"} for every caller, while the token's own allowed_actions
-- said they were permitted.
--
-- Five of them were WIRED in the same card, against the implementation the app already had:
--   contacts.update  <- PUT    /api/contacts/:id        (src/contacts/handlers.rs::update)
--   tags.unassign    <- DELETE /api/tags/assign/:id     (src/tags/handlers.rs::unassign_tag)
--   comms.templates  <- GET    /api/comms/templates     (src/communications/handlers.rs::list_templates)
--   ai.compose       <- POST   /api/ai/message          (src/ai/handlers.rs::compose_message)
--   ai.recommend     <- POST   /api/ai/recommend        (src/ai/handlers.rs::recommend)
--
-- One is RETIRED here because nothing implements it and never did: there is no route, handler or
-- service function that triggers an automation rule on demand. src/automation/engine.rs only
-- evaluates rules from inside the app's own event paths (tag assigned or removed, score changed,
-- list membership) and src/automation/mod.rs exposes rule CRUD only — no "run now". public/guide.html
-- advertises that same CRUD-only surface for /api/automation. Retiring it also closes a contract
-- wart: a webhook token could otherwise name a rule and make the automation engine perform its
-- actions (send email, move stage, add tag) outside the event that is meant to justify them.
--
-- This is a NEW file on purpose. 027_create_affiliate_products.sql is applied and checksummed by
-- sqlx::migrate!, so it must not be edited (see 088's header note). The trigger only fires on
-- INSERT INTO tenants, so the back-fill below is what repairs the workspaces that already carry
-- the stale array — the 81 automation_webhooks rows measured on 2026-09-25.

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
              'affiliates.profile', 'affiliates.referrals', 'affiliates.stats',
              'affiliate_products.list', 'affiliate_products.my', 'affiliate_products.select', 'affiliate_products.unselect',
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

-- Back-fill every row that still advertises the retired action. Idempotent by construction:
-- array_remove is a no-op when the element is absent and the WHERE clause then matches 0 rows.
UPDATE automation_webhooks
   SET allowed_actions = array_remove(allowed_actions, 'automation.trigger'),
       updated_at = NOW()
 WHERE 'automation.trigger' = ANY(allowed_actions);
