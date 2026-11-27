-- Plan requests are a KIND OF TICKET, not a second ticketing system (kanban t_42872c18).
--
-- Before this file the ONLY thing that marked a plan request was the subject string
-- ("Plan request at signup: {plan} plan"), matched in the console with /plan request/i, and the
-- row's source was 'form' — the same value the public contact form writes. So a plan ask could not
-- be queried, counted or told apart from support mail without matching prose, and `tickets` had no
-- column that could hold WHICH plan was asked for.
--
-- Two additions:
--   1. `source = 'plan_request'` becomes a legal value. The CHECK constraint was the only thing
--      stopping it — every other writer already has its own word (manual/email/form/agent/portal/
--      widget/api).
--   2. `plan_slug` records the plan, queryable and joinable to `plans`.
--
-- The fleet contract is /opt/swift/docs/plan-request-queue-contract-2026-11-27.md: the operator
-- queue speaks open -> contacted -> closed, and this app stores open -> pending -> closed.
ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_source_check;
ALTER TABLE tickets
  ADD CONSTRAINT tickets_source_check
  CHECK (source = ANY (ARRAY['manual', 'email', 'form', 'agent', 'portal', 'widget', 'api',
                             'plan_request']));

ALTER TABLE tickets ADD COLUMN IF NOT EXISTS plan_slug text;

-- Backfill: a row the signup door already filed is recognisable by the exact sentence that door
-- writes (0 rows on production 2026-11-27, but a restored clone that has one must land in the same
-- queue). This is the LAST time prose is read to recognise a plan request.
UPDATE tickets
   SET source = 'plan_request'
 WHERE source = 'form'
   AND subject ILIKE 'Plan request at signup:%';

-- The plan's own NAME was the only record of which plan was wanted; recover the slug from the
-- catalogue where the subject names one, so an old row answers with the same shape as a new one.
UPDATE tickets t
   SET plan_slug = p.slug
  FROM plans p
 WHERE t.source = 'plan_request'
   AND t.plan_slug IS NULL
   AND p.name IS NOT NULL
   AND lower(t.subject) LIKE '%' || lower(p.name) || ' plan%';

-- The operator queue lists by this predicate, newest first; a partial index keeps it off the
-- support-desk indexes.
CREATE INDEX IF NOT EXISTS idx_tickets_plan_request
    ON tickets (created_at DESC)
 WHERE source = 'plan_request';
