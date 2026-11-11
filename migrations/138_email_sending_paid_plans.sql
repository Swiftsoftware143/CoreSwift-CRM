-- 138 — REPAIR: every PAID plan sells sending (`email_sending = true`). Free stays false.
--
-- WHAT WENT WRONG IN 136
--   136's paid-plan arm wrote `enabled = (f.key <> 'email_sending')` across four feature keys at
--   once, which is TRUE for the three limit rows and FALSE for the sending row itself. So every paid
--   plan except `inbox` (which 136 wrote with a literal `true`) resolved as "this plan does not sell
--   sending". A paid tenant's mailboxes are `access_mode = 'paid'` with no trial window, so
--   `trial::send_access` saw "not sold" + "no window" and refused their outbound mail with the
--   free-trial sentence — a live regression for every paying customer, caught by the acceptance
--   proof on the same day, minutes after the deploy.
--
-- WHY A NEW FILE AND NOT AN EDIT: 136 is already applied (`_sqlx_migrations`), and sqlx compares a
-- checksum for every applied file — editing it makes the app refuse to serve. Repairs go forward.
--
-- Idempotent: a re-run sets the same value.
UPDATE plan_module_features pf
   SET enabled = true,
       updated_at = now()
  FROM plans p,
       module_features f
 WHERE pf.plan_id = p.id
   AND pf.module_feature_id = f.id
   AND f.key = 'email_sending'
   AND p.slug <> 'free';

-- The invariant this repairs, asserted as data so a future migration cannot quietly undo it: ONLY
-- the free plan withholds sending (the trial grants it per mailbox); every other plan sells it.
DO $$
DECLARE
    wrong int;
BEGIN
    SELECT count(*) INTO wrong
      FROM plan_module_features pf
      JOIN plans p ON p.id = pf.plan_id
      JOIN module_features f ON f.id = pf.module_feature_id
     WHERE f.key = 'email_sending'
       AND p.is_active
       AND p.slug <> 'free'
       AND pf.enabled IS NOT TRUE;
    IF wrong > 0 THEN
        RAISE EXCEPTION 'email_sending is not enabled on % active paid plan(s)', wrong;
    END IF;
END $$;
