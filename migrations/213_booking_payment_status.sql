-- A booking can say whether it has been paid, and a provider can say so.
--
-- Why (ZaarHub via David, 2026-11-25): payment runs through GrooveSell, so the calendar takes the
-- booking and money arrives somewhere else. Until now a booking had no payment state at all — only
-- order status (pending/confirmed/cancelled), which is about the booking, not the money. The loop the
-- sponsor needs is: book a slot -> pay at the provider -> the calendar says "booked and paid".
--
-- Wording is deliberate. Sponsor-facing surfaces say "booked" and never "purchased", "sold" or
-- "bought": a sponsor who has already booked should never be told they bought something. These columns
-- are internal state and are named plainly (unpaid/paid/refunded).
--
-- The reference that survives the round trip is the ORDER id: the booking page appends it to the
-- checkout address as `ref`, and the provider posts it back. A provider that can only echo an id of its
-- own gets `payment_reference` for that, so we can still match on either.

ALTER TABLE booking_orders
    ADD COLUMN IF NOT EXISTS payment_status text NOT NULL DEFAULT 'unpaid';

ALTER TABLE booking_orders
    ADD COLUMN IF NOT EXISTS paid_at timestamp with time zone;

ALTER TABLE booking_orders
    ADD COLUMN IF NOT EXISTS payment_reference text;

ALTER TABLE booking_orders
    ADD COLUMN IF NOT EXISTS payment_provider text;

DO $$
BEGIN
    ALTER TABLE booking_orders
        ADD CONSTRAINT booking_orders_payment_status_chk
        CHECK (payment_status IN ('unpaid', 'paid', 'refunded'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

CREATE INDEX IF NOT EXISTS booking_orders_payment_reference_idx
    ON booking_orders (payment_reference)
    WHERE payment_reference IS NOT NULL;

-- The address a provider posts back to, and the key it must present, are per calendar: an owner pastes
-- them into GrooveSell, and one calendar's key must never settle another calendar's bookings.
ALTER TABLE booking_calendars
    ADD COLUMN IF NOT EXISTS payment_webhook_key text;

COMMENT ON COLUMN booking_orders.payment_status IS
    'unpaid | paid | refunded. Internal state — the sponsor is told "booked", never "purchased".';

COMMENT ON COLUMN booking_calendars.payment_webhook_key IS
    'The key a payment provider must present when it reports a completed sale for this calendar.';
