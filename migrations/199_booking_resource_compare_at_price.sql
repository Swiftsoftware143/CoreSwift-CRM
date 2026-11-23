-- A "was" price on a bookable resource (ZaarHub, 2026-11-23).
--
-- The founders rate needs "$25 instead of $150" on the booking page while the amount CHARGED stays the
-- founders rate. A booking could hold exactly one number, so this is the second one — and it is a
-- DISPLAY figure only: nothing in pricing, quoting, checkout or the ledger may ever read it.
--
-- Nullable on purpose. NULL and "<= price" both mean "show the price alone", which is the rule the
-- page applies, so a half-filled column can never render as a bogus saving.
ALTER TABLE booking_resources
    ADD COLUMN IF NOT EXISTS compare_at_price_cents bigint;

-- A negative "was" price is nonsense and would read as a surcharge.
ALTER TABLE booking_resources
    DROP CONSTRAINT IF EXISTS booking_resources_compare_at_nonneg;
ALTER TABLE booking_resources
    ADD CONSTRAINT booking_resources_compare_at_nonneg
    CHECK (compare_at_price_cents IS NULL OR compare_at_price_cents >= 0);

COMMENT ON COLUMN booking_resources.compare_at_price_cents IS
    'Display-only "was" price. The page strikes it through when it is greater than price_cents; the amount charged is always price_cents.';
