-- 142_email_storage_quota.sql
--
-- STORAGE IS THE PRODUCT RULE, A CLOCK IS NOT (B206, David 2026-11-11).
--
-- The first draft of B206 purged mail on a 90-day clock. David reversed it: hosting can grow, so
-- the app must never delete a customer's mail to make room. What replaces the clock is a STORAGE
-- QUOTA per account, defaulting to 2 GB (not the 5 GB this app used to state in one place and
-- enforce nowhere consistent), scaled by plan so more storage is a real upgrade, with:
--   * a usage meter and an in-app banner on the inbox screen at 50% / 75% / 99% that stays until
--     space is freed, stating the ACTUAL numbers ("1.5 of 2 GB used");
--   * one reminder email to the account owner per threshold per month at most (this table is the
--     dedupe: unique on tenant + threshold + month, claimed BEFORE the mail is queued);
--   * at 100% plain mail is still accepted and nothing is deleted — only NEW attachments/files are
--     refused, and the owner is told to clear space.
--
-- ONE SOURCE FOR THE NUMBER. `email_storage_mb` is the module-registry feature the send path and
-- the console already read for this account (migration 136 registered it; free was 5120 MB and the
-- paid plans were left NULL = uncapped). Leaving the paid plans uncapped would mean the meter on a
-- paid plan could never reach a threshold, so this migration gives EVERY plan an explicit
-- allowance: the ladder is the upgrade ladder. The retired `plans.features.storage_gb` key is NOT
-- read here (migration 103 retired it as unenforceable, and its free value is 0.1 GB, a number this
-- app never promised a mailbox); the registry row is the single authority.
--
-- THE TIME-BASED RETENTION IS DEMOTED TO A FAR BACKSTOP, NOT REMOVED. `tenant_email_limits`
-- keeps `retention_days`, and the admin panel keeps its 30..3650 control. The DEFAULT moves from
-- 365 days to 3650 (ten years) and every existing override shorter than that is raised to the
-- backstop, so no customer mail is deleted on a clock by this deploy. The inbox screen states the
-- backstop with the account's real number whenever it applies.

-- ── 1. The storage allowance, per plan, in MB ─────────────────────────────────────────────────
-- Monotonic with price, so an upgrade always buys more room: free 2 GB, the $1 inbox 5 GB,
-- starter 10 GB, professional 50 GB, enterprise 100 GB, agency 256 GB. `pro` is the inactive old
-- starter twin and carries the starter allowance so activating it cannot undercut Starter.
UPDATE plan_module_features pf
   SET enabled = true,
       limit_value = v.mb,
       updated_at = now()
  FROM plans p,
       module_features f,
       (VALUES
         ('free',         2048::numeric),
         ('inbox',        5120::numeric),
         ('starter',     10240::numeric),
         ('pro',         10240::numeric),
         ('professional',51200::numeric),
         ('enterprise', 102400::numeric),
         ('agency',     256000::numeric)
       ) AS v(slug, mb)
 WHERE pf.plan_id = p.id
   AND pf.module_feature_id = f.id
   AND f.key = 'email_storage_mb'
   AND p.slug = v.slug;

-- A plan that somehow has no row yet (a plan added after 136) gets one at its ladder value. This
-- only inserts for the slugs above; an unknown plan is left alone rather than guessed at.
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, true, v.mb
  FROM plans p
  JOIN module_features f ON f.key = 'email_storage_mb'
  JOIN (VALUES
         ('free',         2048::numeric),
         ('inbox',        5120::numeric),
         ('starter',     10240::numeric),
         ('pro',         10240::numeric),
         ('professional',51200::numeric),
         ('enterprise', 102400::numeric),
         ('agency',     256000::numeric)
       ) AS v(slug, mb) ON v.slug = p.slug
 WHERE NOT EXISTS (
        SELECT 1 FROM plan_module_features pf
         WHERE pf.plan_id = p.id AND pf.module_feature_id = f.id
      );

-- ── 2. The reminder ledger: one message per threshold per month, at most ──────────────────────
-- The row is claimed BEFORE the mail is queued (INSERT .. ON CONFLICT DO NOTHING RETURNING id), so
-- two inbound deliveries crossing 75% at the same second cannot both queue a message. `period` is
-- the UTC month ('YYYY-MM'): a new month re-arms the threshold, which is why the mail informs
-- rather than nags.
CREATE TABLE IF NOT EXISTS private_email_storage_notices (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id     uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- 50, 75 and 99 (a CHECK, not a free number: the banner and the mail are the same three rungs).
    threshold_pct smallint NOT NULL CHECK (threshold_pct IN (50, 75, 99)),
    -- UTC month, 'YYYY-MM'.
    period        varchar(7) NOT NULL,
    usage_bytes   bigint NOT NULL,
    quota_bytes   bigint NOT NULL,
    sent_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, threshold_pct, period)
);

CREATE INDEX IF NOT EXISTS idx_private_email_storage_notices_tenant
    ON private_email_storage_notices (tenant_id, sent_at DESC);

-- ── 3. Retention becomes a far backstop ──────────────────────────────────────────────────────
-- The column default moves to ten years, and every existing override shorter than the backstop is
-- raised: a 365-day row that nobody remembers setting must not start deleting mail the day this
-- ships. An admin may still set a shorter window deliberately (the 30..3650 control), and the
-- inbox screen states whatever that window is.
ALTER TABLE tenant_email_limits
    ALTER COLUMN retention_days SET DEFAULT 3650;

UPDATE tenant_email_limits
   SET retention_days = 3650
 WHERE retention_days < 3650;
