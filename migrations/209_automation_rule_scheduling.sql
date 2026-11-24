-- 209 — an automation rule can WAIT, check a condition, and be scheduled by the clock.
--
-- Measured 2026-11-24 (kanban t_990c386e): a rule could only run AT THE INSTANT its trigger fired.
-- `delayed_actions` already existed as the If-Not-Then queue (condition_type timeout|no_event|
-- no_action, executed every 60s by src/worker.rs), but nothing tied a queued row back to the RULE
-- that produced it, so three things were impossible:
--
--   * a rule with a delay step — the engine had nowhere to put the wait, so it had to either fire
--     immediately or not at all;
--   * an honest execution_count — the counter is incremented by the engine when it executes, and a
--     row executed later by the worker had no way to find its rule;
--   * a scan-based trigger (a renewal date entering its window, a contact going quiet) — those fire
--     on the clock, not on an event, and need a per-(rule, contact) ledger so the same contact is
--     not re-queued on every pass.
--
-- `source_rule_id` is that link. It is nullable because `delayed_actions` has other writers
-- (monitoring, checklists, private-email auto-reply, POST /api/events/delayed) that have no rule;
-- ON DELETE SET NULL so deleting a rule does not destroy the history of what it queued.
--
-- The index is partial: the worker's due-queue query is served by
-- idx_delayed_actions_execute, and the only lookups this column serves are "did this rule already
-- queue this entity" (the scan ledger), which never needs non-rule rows.

ALTER TABLE public.delayed_actions
    ADD COLUMN IF NOT EXISTS source_rule_id uuid
        REFERENCES public.automation_rules(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_delayed_actions_rule_entity
    ON public.delayed_actions USING btree (source_rule_id, entity_id)
    WHERE (source_rule_id IS NOT NULL);

COMMENT ON COLUMN public.delayed_actions.source_rule_id IS
    'The automation_rules row that queued this action (NULL for monitoring/checklist/API writers). '
    'Also the scan-trigger ledger: a scan trigger does not re-queue a contact while a row for the '
    'same (source_rule_id, entity_id) is inside its recheck window (kanban t_990c386e).';
