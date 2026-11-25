-- A recurring price has to say how often, and a field this endpoint does not take has to say so.
--
-- Why (ZaarHub via David, 2026-11-25): "priceType: 'recurring' saves with no interval. Nine field names
-- tried, all silently dropped, every one returning 200." Both halves of that are bugs:
--
--   1. There was no column for a billing interval at all, so a recurring price could be saved with
--      nothing saying how often it recurs — a subscription that bills on a schedule nobody recorded.
--   2. The request structs accepted any key and dropped what they did not recognise, so nine wrong names
--      each returned 200. A 200 that saves nothing is worse than an error: it looks like it worked.
--
-- The columns are additive and nullable: existing plans keep their monthly/yearly prices, and a plan only
-- carries an interval once someone actually sets one. The refusal side is in the handlers
-- (`deny_unknown_fields` on the plan requests plus an explicit check), because "interval missing" is a
-- question about the payload, not about the schema.

ALTER TABLE plans
    ADD COLUMN IF NOT EXISTS price_type text;

ALTER TABLE plans
    ADD COLUMN IF NOT EXISTS billing_interval text;

ALTER TABLE plans
    ADD COLUMN IF NOT EXISTS billing_interval_count integer;

DO $$
BEGIN
    ALTER TABLE plans
        ADD CONSTRAINT plans_price_type_chk
        CHECK (price_type IS NULL OR price_type IN ('one_time', 'recurring'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$
BEGIN
    ALTER TABLE plans
        ADD CONSTRAINT plans_billing_interval_chk
        CHECK (billing_interval IS NULL
               OR billing_interval IN ('day', 'week', 'month', 'year'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$
BEGIN
    ALTER TABLE plans
        ADD CONSTRAINT plans_billing_interval_count_chk
        CHECK (billing_interval_count IS NULL OR billing_interval_count > 0);
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

COMMENT ON COLUMN plans.price_type IS
    'one_time | recurring. A recurring price must also carry billing_interval — refused otherwise.';

COMMENT ON COLUMN plans.billing_interval IS
    'day | week | month | year. The unit of one billing period.';

COMMENT ON COLUMN plans.billing_interval_count IS
    'How many billing_interval units per period (e.g. 3 + month = quarterly).';
