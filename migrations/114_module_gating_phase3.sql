-- CoreSwift-CRM: Phase-3 specialist-module plan gating — the DATA half (migration 114).
--
-- WHY (David 2026-10-06 Phase 3 table; verified live by kanban t_e22c1338, implemented here by t_9b2e0c3e):
-- 18 of the 22 registry modules carried `plan_modules.enabled = true` on EVERY plan including `free`,
-- so a gate wired on any of them could never deny anything. This file is the seating decision that
-- makes the wired gates able to say no; the matching code half is in the same commit
-- (`src/features.rs` keys + the call sites in bookings / scoring / notifications / integrations /
-- automation engine).
--
-- TIER MAP (decided on the card, not re-opened here). Live plan slugs, cheapest first:
--   free(0) -> starter(29) -> pro(29) -> professional(79) -> enterprise(79) -> agency(149)
-- Light = starter; Pro = pro + professional; Enterprise = enterprise + agency. `free` is the 0-tier
-- and keeps only what the table says "everyone" gets.
--
-- ADDITIVE, like 072/113: no `plans` row, column, price, name or `plans.features` key is touched.
-- Every statement is idempotent, and every plan x module / plan x feature cell it writes already has a
-- row (072 seeded a full matrix), so the DO UPDATE arms are what change the seating. The assignment
-- tables are the ones the admin's Features & Plans panel writes, so everything below is reversible
-- with one press per cell — no deploy.

-- ─────────────────────────────────────────────────────────────────────────────────────────────
-- 1. ROUND-ROBIN — "Enterprise only". The gate is already wired (src/round_robin/mod.rs:40, key
--    `round_robin`); only the data opened it. Live before: a free tenant created a team (201).
-- ─────────────────────────────────────────────────────────────────────────────────────────────
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, (p.slug IN ('enterprise', 'agency'))
  FROM plans p
  CROSS JOIN modules m
 WHERE m.key = 'round_robin'
ON CONFLICT (plan_id, module_id)
DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now();

-- ─────────────────────────────────────────────────────────────────────────────────────────────
-- 2. WEBHOOKS — "Enterprise only". The `webhooks` module row existed and NOTHING read it; the code
--    half (this commit) puts `gate_mw` with key `webhooks` on the tenant-facing webhook CONFIG
--    surface, `/api/integrations/webhooks`. The public automation webhook
--    (`POST /api/webhook/:token/:action`) stays token-auth by design and is deliberately NOT gated.
-- ─────────────────────────────────────────────────────────────────────────────────────────────
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, (p.slug IN ('enterprise', 'agency'))
  FROM plans p
  CROSS JOIN modules m
 WHERE m.key = 'webhooks'
ON CONFLICT (plan_id, module_id)
DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now();

-- ─────────────────────────────────────────────────────────────────────────────────────────────
-- 3. AUTOMATIONS — the "Enterprise automations" half of the Tags row. Tag CRUD itself stays open to
--    everyone (it matches "Tags — everyone" and nothing gates it), but the tag FAN-OUT
--    (`automation::engine::evaluate_tag_triggers`, reached from the tag paths rather than from the
--    /api/automation router) must be refused without this module. Live before: a free tenant read
--    /api/automation/rules 200 because `automation` was true on all six plans.
--    The card's decided seating: pro and up (the table's "Pro" and "Enterprise" columns).
-- ─────────────────────────────────────────────────────────────────────────────────────────────
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, (p.slug IN ('pro', 'professional', 'enterprise', 'agency'))
  FROM plans p
  CROSS JOIN modules m
 WHERE m.key = 'automation'
ON CONFLICT (plan_id, module_id)
DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now();

-- 072 section 4: keep each module's own boolean feature row in step with its module assignment, so
-- the admin matrix shows the module and its feature agreeing for these three keys.
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT pm.plan_id, f.id, pm.enabled, NULL
  FROM plan_modules pm
  JOIN modules m ON m.id = pm.module_id
  JOIN module_features f ON f.module_id = m.id AND f.key = m.key
 WHERE m.key IN ('webhooks', 'round_robin', 'automation')
ON CONFLICT (plan_id, module_feature_id)
DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now();

-- ─────────────────────────────────────────────────────────────────────────────────────────────
-- 4. NOTIFICATIONS — "Pro basic / Enterprise omnichannel". No module row existed at all, so the
--    queue accepted EVERY channel on `free` (proven live: email / sms / whatsapp / in_app all 201).
--
--    SHAPE, and where it deliberately departs from the card: the card asked for one
--    `notifications_channels` feature of kind=limit, unit=channels. A COUNT cannot express this
--    feature — basic is a NAMED pair (in_app + email) and the required refusal has to NAME the
--    channel it is refusing ("SMS notifications is not available on your current plan"). A numeric
--    cap would also be a registered limit that no code path compares against, i.e. a panel knob that
--    cannot fire. So the channel entitlement is registered the way this registry expresses a
--    per-capability grant: one boolean per CHANNEL, grouped under the `notifications` module.
--      * in_app + email  -> Pro and up   (the table's "Pro basic")
--      * sms + whatsapp  -> Enterprise and up (the table's "Enterprise omnichannel")
--      * free, starter   -> nothing (the 0/entry tiers; the table gives notifications only to Pro+)
--    Reads (`GET /api/notifications`, `/unread-count`) stay ungated — they are not a sold capability.
-- ─────────────────────────────────────────────────────────────────────────────────────────────
INSERT INTO modules (key, name, description, icon, sort_order, legacy_feature_key)
VALUES (
    'notifications',
    'Notifications',
    'Notify contacts and the team over in-app, email, SMS and WhatsApp (all four channels on Enterprise)',
    '🔔',
    240,
    NULL
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO module_features (module_id, key, name, kind, unit, sort_order, legacy_feature_key)
SELECT m.id, v.key, v.name, 'boolean', NULL, v.so, NULL
  FROM modules m
  CROSS JOIN (VALUES
        ('notifications',           'Notifications',            0),
        ('notifications_in_app',    'In-app notifications',     1),
        ('notifications_email',     'Email notifications',      2),
        ('notifications_sms',       'SMS notifications',        3),
        ('notifications_whatsapp',  'WhatsApp notifications',   4)
    ) AS v(key, name, so)
 WHERE m.key = 'notifications'
ON CONFLICT (key) DO NOTHING;

-- The module row itself, and the basic two channels: Pro and up.
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, (p.slug IN ('pro', 'professional', 'enterprise', 'agency'))
  FROM plans p
  CROSS JOIN modules m
 WHERE m.key = 'notifications'
ON CONFLICT (plan_id, module_id)
DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now();

INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, (p.slug IN ('pro', 'professional', 'enterprise', 'agency')), NULL
  FROM plans p
  CROSS JOIN module_features f
 WHERE f.key IN ('notifications', 'notifications_in_app', 'notifications_email')
ON CONFLICT (plan_id, module_feature_id)
DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now();

-- Omnichannel: SMS + WhatsApp are Enterprise and up.
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, (p.slug IN ('enterprise', 'agency')), NULL
  FROM plans p
  CROSS JOIN module_features f
 WHERE f.key IN ('notifications_sms', 'notifications_whatsapp')
ON CONFLICT (plan_id, module_feature_id)
DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now();

-- ─────────────────────────────────────────────────────────────────────────────────────────────
-- 5. BOOKINGS — "Light 1 / Pro 5 / Enterprise unlimited". Everyone can still BOOK (the `bookings`
--    module stays true on all six plans — that is what the table says), but the number of CALENDARS a
--    workspace may own was unbounded. Live before: a free tenant created two calendars, 201/201.
--    New limit row inside the `limits` module, enforced on the calendar ADD path only.
--    `-1` is this app's own unlimited sentinel (usage_ceiling maps a negative to "no ceiling").
-- ─────────────────────────────────────────────────────────────────────────────────────────────
INSERT INTO module_features (module_id, key, name, kind, unit, sort_order, legacy_feature_key)
SELECT m.id, 'limit_bookings', 'Booking calendars', 'limit', 'calendars', 100, NULL
  FROM modules m
 WHERE m.key = 'limits'
ON CONFLICT (key) DO NOTHING;

-- ─────────────────────────────────────────────────────────────────────────────────────────────
-- 6. SCORING RULES — "Pro 3 / Enterprise unlimited". The `ai_enabled` module gate already refuses
--    free/starter (proven live: 402 on /api/scoring/rules), but a plan that HAS scoring was
--    unbounded — one live tenant holds 7 rules. `0` is this registry's encoding for "not included on
--    this plan" (see `email_domains` free), and `-1` is unlimited.
-- ─────────────────────────────────────────────────────────────────────────────────────────────
INSERT INTO module_features (module_id, key, name, kind, unit, sort_order, legacy_feature_key)
SELECT m.id, 'limit_scoring_rules', 'Scoring rules', 'limit', 'rules', 101, NULL
  FROM modules m
 WHERE m.key = 'limits'
ON CONFLICT (key) DO NOTHING;

INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, true, v.lv
  FROM plans p
  JOIN (VALUES
        ('free',         'limit_bookings',       1),
        ('starter',      'limit_bookings',       1),
        ('pro',          'limit_bookings',       5),
        ('professional', 'limit_bookings',       5),
        ('enterprise',   'limit_bookings',      -1),
        ('agency',       'limit_bookings',      -1),
        ('free',         'limit_scoring_rules',  0),
        ('starter',      'limit_scoring_rules',  0),
        ('pro',          'limit_scoring_rules',  3),
        ('professional', 'limit_scoring_rules',  3),
        ('enterprise',   'limit_scoring_rules', -1),
        ('agency',       'limit_scoring_rules', -1)
    ) AS v(slug, fkey, lv) ON v.slug = p.slug
  JOIN module_features f ON f.key = v.fkey
ON CONFLICT (plan_id, module_feature_id)
DO UPDATE SET enabled = EXCLUDED.enabled, limit_value = EXCLUDED.limit_value, updated_at = now();

-- ─────────────────────────────────────────────────────────────────────────────────────────────
-- NOT CHANGED, on purpose — the same commit's report carries the reasoning:
--
--   * PIPELINES — the table says "Pro simple / Enterprise full", and NO simple/full axis exists in
--     either the schema or the code (no module row, no column, no branch); only the count
--     `limit_pipelines` (1/2/5/5/20/100). It is left as the count it is: a maximum number of
--     pipelines, enforced on `POST /api/pipelines` only. Emptying `free`'s single pipeline would be
--     inventing an axis the product does not have, so the axis is reported as ABSENT instead.
--   * the public automation webhook `POST /api/webhook/:token/:action` — token-auth by design.
--   * `/api/scoring/webhooks` — scoring's OWN webhook targets, already gated by `ai_enabled` (Pro+).
--     Putting the `webhooks` key here too would take a feature away from Pro and Professional, which
--     the table sells as part of Pro.
-- ─────────────────────────────────────────────────────────────────────────────────────────────
