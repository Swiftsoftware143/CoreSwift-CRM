-- Where a booking came from, kept with the booking.
--
-- Why (ZaarHub via David, 2026-11-25): tracking.zaarhub.com sells through a tracking link, and a
-- sponsor's issue has to be traceable back to the source that sold it. Two different things must not be
-- confused: the tracking LINK is attribution (which campaign brought the buyer), and the sponsor's
-- REPORT (views, clicks, which issue) comes from the placement itself, not from here.
--
-- The page carries a `?t=` (or `?tracking=`) value into the order, so the link that produced a booking
-- survives even when the payment is taken somewhere else.

ALTER TABLE booking_orders
    ADD COLUMN IF NOT EXISTS tracking_url text;

COMMENT ON COLUMN booking_orders.tracking_url IS
    'The tracking link (or short code) that produced this booking. Attribution only — the sponsor''s '
    'own reporting comes from the placement, not from this column.';
