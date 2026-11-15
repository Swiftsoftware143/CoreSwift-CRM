-- The helpdesk module joins the plan registry, and self-learning becomes a thing a plan SELLS.
--
-- David's question (2026-11-14): "is this a feature that I can check depending on the payment plan?" It has to be,
-- so it is DATA in the same registry the admin panel already edits — not a constant in the code. Adding the
-- feature key here means a plan can be changed without a redeploy, a single account can be given it as an
-- override, and the refusal has a sentence to show ("Upgrade required: …").
--
-- WHAT IS GATED: harvesting solved tickets into draft articles, and the review queue that shows them. Writing
-- articles by hand, the public help centre, the widget search and the AI drafting from a description are NOT
-- gated here — the free desk keeps everything it already had.
--
-- WHO GETS IT: every paid plan. Free does not, deliberately: it is the one feature whose value grows with
-- VOLUME of answered conversations, which is exactly what a paid desk has.
--
-- Idempotent: ON CONFLICT everywhere, so replaying this file cannot double-grant or re-shift the ladder.

-- ── 1. The module ──────────────────────────────────────────────────────────────────────────
INSERT INTO modules (key, name, description, icon, sort_order)
VALUES ('helpdesk', 'Helpdesk', 'Tickets, knowledge base and the customer portal', 'headset', 60)
ON CONFLICT (key) DO NOTHING;

-- ── 2. The feature ─────────────────────────────────────────────────────────────────────────
INSERT INTO module_features (module_id, key, name, description, kind, sort_order)
SELECT m.id, 'helpdesk_kb_learning', 'Learns from solved tickets',
       'Drafts articles from conversations that were already answered, for a person to review and publish',
       'boolean', 10
  FROM modules m WHERE m.key = 'helpdesk'
ON CONFLICT (key) DO NOTHING;

-- ── 3. Which plans sell it ─────────────────────────────────────────────────────────────────
-- Every plan that is sold, except Free. `inbox` is the $1 standalone mailbox and is not a desk, so it does not get
-- it either.
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, p.slug <> 'free'
  FROM plans p, modules m
 WHERE m.key = 'helpdesk'
ON CONFLICT (plan_id, module_id) DO UPDATE SET enabled = EXCLUDED.enabled;

INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, p.slug NOT IN ('free', 'inbox'), NULL
  FROM plans p, module_features f
 WHERE f.key = 'helpdesk_kb_learning'
ON CONFLICT (plan_id, module_feature_id) DO UPDATE SET enabled = EXCLUDED.enabled;

-- ── 4. Prove it, from the data ─────────────────────────────────────────────────────────────
DO $$
DECLARE
    granted int;
    refused int;
BEGIN
    SELECT count(*) INTO granted FROM plan_module_features pf
      JOIN module_features f ON f.id = pf.module_feature_id
     WHERE f.key = 'helpdesk_kb_learning' AND pf.enabled;
    SELECT count(*) INTO refused FROM plan_module_features pf
      JOIN module_features f ON f.id = pf.module_feature_id
     WHERE f.key = 'helpdesk_kb_learning' AND NOT pf.enabled;
    IF granted = 0 THEN
        RAISE EXCEPTION 'helpdesk_kb_learning is enabled on no plan at all';
    END IF;
    RAISE NOTICE 'helpdesk_kb_learning: % plan(s) sell it, % refuse it', granted, refused;
END $$;
