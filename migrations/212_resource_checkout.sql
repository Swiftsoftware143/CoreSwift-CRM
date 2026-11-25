-- The checkout belongs to the PRODUCT, not just the calendar.
--
-- Why (David, 2026-11-24): "that type of ad will have to be mapped to the proper checkout... we have
-- multiple different newsletters and it's priced differently. So maybe it should be somewhere where we
-- create products and then those products get added to the calendar... and then in the product section
-- is where we connected to a certain checkout link or embed."
--
-- So a position carries its own checkout code and its own payment timing, and the calendar's values are
-- the DEFAULT for positions that do not set their own. An order that mixes positions with different
-- checkouts shows one embedded form per distinct checkout rather than charging one product's price
-- through another product's till.

ALTER TABLE booking_resources
    ADD COLUMN IF NOT EXISTS checkout_embed text;

ALTER TABLE booking_resources
    ADD COLUMN IF NOT EXISTS payment_mode text;

DO $$
BEGIN
    ALTER TABLE booking_resources
        ADD CONSTRAINT booking_resources_payment_mode_chk
        CHECK (payment_mode IS NULL OR payment_mode IN ('before', 'after'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

COMMENT ON COLUMN booking_resources.checkout_embed IS
    'This product''s own checkout code or link. Only its https address is read; nothing in it runs on '
    'the customer page. Empty falls back to the calendar''s checkout.';

COMMENT ON COLUMN booking_resources.payment_mode IS
    'Per-product timing: "before" secures the booking, "after" is paid on the confirmation. NULL falls '
    'back to the calendar, then to the calendar kind.';
