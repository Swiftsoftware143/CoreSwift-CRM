-- CoreSwift — retire `ada_campaign_triggers`: a table, three routes and a mapping nothing read.
--
-- MEASURED LIVE 2026-10-02 (kanban t_434b240b), before anything was touched:
--   * `ada_campaign_triggers` held 0 rows.
--   * `grep -rn 'trigger_on' src/` hit ONLY the CRUD handlers + the model: NO code path ever read
--     the mapping, so a row could never fire a campaign.
--   * live probe on a throwaway tenant: creating a trigger for `contact_created` (201) then driving
--     `contact_created` + a tag assign produced NO outbound call — a local sink that the app's own
--     manual AdaSwift push (entity_type `trigger_campaign`) DOES reach stayed at 2 hits — and no
--     dispatcher artefact appeared (`app_sync_logs` unchanged).
--   * the four affiliate event names the validator still accepted (`referral_confirmed`,
--     `commission_earned`, `payout_processed`, `affiliate_activated`) had no emitter at all: the
--     affiliate module was deleted by t_3d81b041 and migration 108 dropped its tables.
--   * the destination has no receiver: AdaSwift answers `POST /api/campaigns/trigger` 404.
--
-- The mechanism has been removed from the code (handlers, routes, models, the served console's Ada
-- card and the TASKS / VERIFICATION claims). This file drops the table and the CHECK constraint
-- that still advertised the four retired affiliate names.
--
-- SAFETY — this cannot destroy data it did not measure: the drop runs ONLY while the table is
-- EMPTY; if it ever holds a row the migration RAISES and ABORTS.
DO $$
DECLARE
    n bigint;
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.tables
        WHERE table_schema = 'public' AND table_name = 'ada_campaign_triggers'
    ) THEN
        EXECUTE 'SELECT count(*) FROM ada_campaign_triggers' INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION
                'refusing to drop ada_campaign_triggers: it holds % row(s). This migration was measured against an EMPTY table; a populated one means something now uses it and it must be wired or retired deliberately.',
                n;
        END IF;
        DROP TABLE IF EXISTS ada_campaign_triggers CASCADE;
    END IF;
END $$;
