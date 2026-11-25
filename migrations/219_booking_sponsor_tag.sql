-- A BOOKING CAN TAG ITS SPONSOR (ZaarHub CEO, 2026-11-25).
--
-- The sponsor flow was two halves that never met: the booking form captured a buyer, and the CRM had 412
-- tags and ten "‹City› - Sponsors" lists built on them with zero members. Nothing connected them.
--
-- One column on the calendar holds the tag name, so ten calendars carry ten tags with no code branching.
-- Empty/null means "this calendar does not tag its bookings" — a booking must never fail because a
-- calendar has not been given a tag yet.
ALTER TABLE booking_calendars
    ADD COLUMN IF NOT EXISTS sponsor_tag text;

COMMENT ON COLUMN booking_calendars.sponsor_tag IS
    'Tag name applied to the CRM contact created from this calendar''s bookings, e.g. "Palm Bay - Sponsor". '
    'Null/empty means no tagging. Read at booking time; the tag must already exist in this account.';

-- Who booked, when we knew. `booking_orders.contact_id` already existed but nothing ever filled it, so an
-- operator looking at an order could not reach the sponsor's CRM record.
COMMENT ON COLUMN booking_orders.contact_id IS
    'The CRM contact this booking created or matched (by email), filled at booking time since 2026-11-25.';
