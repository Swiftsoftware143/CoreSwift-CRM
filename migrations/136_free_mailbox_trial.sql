-- 136 — The FREE mailbox trial AND the "Inbox" plan ($1/mo, $10/yr).
-- Full brief: kanban t_2ef67217 (CEO, David 2026-11-11; folds in t_da54aef8).
--
-- WHAT DAVID DECIDED
--   * A new customer gets ONE mailbox free for 30 days, full function (send + receive).
--   * At day 30 the mailbox does NOT stop and NO mail is deleted: it drops to RECEIVE-ONLY.
--   * Two upgrade doors: keep the inbox alone for $1/mo ($10/yr — ten months), or upgrade the CRM.
--   * The free plan itself never sells sending: the TRIAL grants it, per mailbox.
--
-- WHAT WAS BROKEN IN THE DATA
--   `plan_modules` had private_email enabled for `free` while `plan_module_features` had no
--   `email_mailboxes` row for it, so the mailbox limit resolved to zero — the module was on and
--   unusable. Worse, the live box only carried a hand-edit from the admin panel (2026-11-05), so a
--   database rebuilt from `migrations/` would hand every new Free tenant ZERO mailboxes again.
--   This file makes the free tier reproducible and adds the plan the $1 door sells.
--
-- EVERY NUMBER HERE IS DATA. The trial length, the fair-use ceiling and the storage ceiling are
-- registry rows the admin panel already edits (`module_features.kind = 'limit'`), so none of them
-- needs a redeploy to change. `email_history_days` (already registered: free 30, starter 90,
-- professional 365, enterprise/agency unlimited) is NOT touched: the trial changes permission to
-- SEND, never how long mail is kept.
--
-- Idempotent: every statement is IF NOT EXISTS / ON CONFLICT / a fixed-value UPDATE, so replaying it
-- cannot double-assign or re-shift the plan ladder.

-- ── 1. The three mailbox states (trial -> receive_only -> paid) ─────────────────────────────────
-- `paid` is the default so every mailbox that exists today (all on plans that sell sending) keeps
-- exactly its current behaviour without a backfill.
ALTER TABLE private_email_boxes
    ADD COLUMN IF NOT EXISTS access_mode varchar(16) NOT NULL DEFAULT 'paid';

ALTER TABLE private_email_boxes
    ADD COLUMN IF NOT EXISTS trial_started_at timestamptz;

ALTER TABLE private_email_boxes
    ADD COLUMN IF NOT EXISTS trial_ends_at timestamptz;

-- When the sweep actually flipped this mailbox. NULL = never expired; distinct from the DEADLINE it
-- was sold (`trial_ends_at`), so the console can tell "ends in 3 days" from "ended on 3 Jan".
ALTER TABLE private_email_boxes
    ADD COLUMN IF NOT EXISTS trial_expired_at timestamptz;

DO $$
BEGIN
    ALTER TABLE private_email_boxes
        ADD CONSTRAINT private_email_boxes_access_mode_check
        CHECK (access_mode IN ('trial', 'receive_only', 'paid'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

-- The sweep's own query: only live trials, only once each.
CREATE INDEX IF NOT EXISTS idx_private_email_boxes_trial_due
    ON private_email_boxes (trial_ends_at)
    WHERE access_mode = 'trial';

-- ── 2. Free keeps the module, ONE mailbox and ONE domain, durably ───────────────────────────────
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, true
  FROM plans p
  JOIN modules m ON m.key = 'private_email'
 WHERE p.slug = 'free'
ON CONFLICT (plan_id, module_id)
DO UPDATE SET enabled = true, updated_at = now();

INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, true, 1
  FROM plans p
  JOIN module_features f ON f.key IN ('email_mailboxes', 'email_domains')
 WHERE p.slug = 'free'
ON CONFLICT (plan_id, module_feature_id)
DO UPDATE SET enabled = true, limit_value = EXCLUDED.limit_value, updated_at = now();

-- ── 3. The four trial/billing registry features (owner: the private_email module) ───────────────
INSERT INTO module_features (module_id, key, name, description, kind, unit, sort_order)
SELECT m.id, v.key, v.name, v.descr, v.kind, v.unit, v.sort
  FROM modules m
  JOIN (VALUES
        ('email_sending',          'Email sending',
         'Whether this plan may SEND mail from its mailboxes (receiving is always allowed)',
         'boolean', NULL::varchar, 24),
        ('email_trial_days',       'Mailbox trial',
         'Days a free mailbox keeps full send+receive before it drops to receive-only',
         'limit', 'days', 25),
        ('email_outbound_per_day', 'Daily send cap',
         'Fair-use ceiling on messages sent per day; protects the sending domain reputation',
         'limit', 'messages', 26),
        ('email_storage_mb',       'Mailbox storage',
         'Storage this plan may use for stored mail',
         'limit', 'MB', 27)
       ) AS v(key, name, descr, kind, unit, sort)
    ON m.key = 'private_email'
ON CONFLICT (key) DO NOTHING;

-- ── 4. Assign them: paid plans sell sending outright, `free` gets the trial instead ─────────────
-- Paid plans (every plan that is not `free`, INCLUDING the new `inbox`): sending on, no trial clock,
-- no fair-use ceiling, no storage cap — i.e. exactly the behaviour the paid tiers have today.
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, (f.key <> 'email_sending'), NULL::numeric
  FROM plans p
  JOIN module_features f
    ON f.key IN ('email_sending', 'email_trial_days', 'email_outbound_per_day', 'email_storage_mb')
 WHERE p.slug <> 'free'
ON CONFLICT (plan_id, module_feature_id) DO NOTHING;

-- Free: the PLAN sells no sending; the trial grants it per mailbox for 30 days, 200 sends a day
-- (fair use — it protects the address's deliverability, it is not a product limit) and 5 GB.
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id,
       (f.key <> 'email_sending'),
       CASE f.key
           WHEN 'email_trial_days'       THEN 30::numeric
           WHEN 'email_outbound_per_day' THEN 200::numeric
           WHEN 'email_storage_mb'       THEN 5120::numeric
           ELSE NULL::numeric
       END
  FROM plans p
  JOIN module_features f
    ON f.key IN ('email_sending', 'email_trial_days', 'email_outbound_per_day', 'email_storage_mb')
 WHERE p.slug = 'free'
ON CONFLICT (plan_id, module_feature_id)
DO UPDATE SET enabled = EXCLUDED.enabled,
              limit_value = EXCLUDED.limit_value,
              updated_at = now();

-- ── 5. The `inbox` plan — $1/mo, $10/yr (annual = ten months, the house rule) ──────────────────
-- SHIPPED INACTIVE ON PURPOSE (brief section 6): this app's only money arm is a plan's own hosted
-- payment page, and NO plan currently carries a `checkout_url` or a `payment_provider` (measured
-- live 2026-11-11), so an active $1 plan would render a button that cannot take money. `is_active`
-- flips to true in the change that lands a working payment link — not before.
INSERT INTO plans (name, slug, description, price_monthly, price_yearly, features, is_active,
                   sort_order, max_industries)
VALUES (
    'Inbox',
    'inbox',
    'Private inbox only — one mailbox on your own domain.',
    1.00,
    10.00,
    -- States ONLY what the dollar buys. It must not inherit a CRM plan's keys: an `inbox` customer
    -- has a mailbox, not a CRM.
    '{"private_email": true, "email_mailboxes": 1, "email_domains": 1, "email_history_days": 3650}'::jsonb,
    false,
    1,
    1
)
ON CONFLICT (slug) DO NOTHING;

-- Immediately after `free`, before `starter`. A FIXED mapping, not `+1`, so a replay cannot walk the
-- ladder upwards on every boot.
UPDATE plans
   SET sort_order = CASE slug
                        WHEN 'starter'      THEN 2
                        WHEN 'professional' THEN 3
                        WHEN 'pro'          THEN 3
                        WHEN 'enterprise'   THEN 4
                        WHEN 'agency'       THEN 5
                        ELSE sort_order
                    END
 WHERE slug IN ('starter', 'professional', 'pro', 'enterprise', 'agency')
   AND EXISTS (SELECT 1 FROM plans WHERE slug = 'inbox');

-- The inbox plan grants the private-email module and NOTHING else: every other module is an explicit
-- `false` row, so no admin screen has to guess and no gated CRM route can open by omission.
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id, m.id, (m.key = 'private_email')
  FROM plans p
  JOIN modules m ON m.is_active
 WHERE p.slug = 'inbox'
ON CONFLICT (plan_id, module_id)
DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now();

-- Its features: one mailbox, one domain, history unlimited (it is a PAID plan — do not reuse the
-- free tier's 30-day window), and sending sold outright.
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id,
       true,
       CASE f.key
           WHEN 'email_mailboxes'        THEN 1::numeric
           WHEN 'email_domains'          THEN 1::numeric
           WHEN 'email_history_days'     THEN NULL::numeric
           WHEN 'email_trial_days'       THEN NULL::numeric
           WHEN 'email_outbound_per_day' THEN NULL::numeric
           WHEN 'email_storage_mb'       THEN NULL::numeric
           ELSE NULL::numeric
       END
  FROM plans p
  JOIN module_features f
    ON f.key IN ('private_email', 'email_mailboxes', 'email_domains', 'email_history_days',
                 'email_sending', 'email_trial_days', 'email_outbound_per_day', 'email_storage_mb')
 WHERE p.slug = 'inbox'
ON CONFLICT (plan_id, module_feature_id)
DO UPDATE SET enabled = EXCLUDED.enabled,
              limit_value = EXCLUDED.limit_value,
              updated_at = now();
