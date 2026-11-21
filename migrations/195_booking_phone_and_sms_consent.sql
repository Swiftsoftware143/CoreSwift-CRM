-- PHONE AND TEXT CONSENT ON A BOOKING (David, 2026-11-21)
--
-- The booking form now asks for an optional phone number and an explicit text-message consent. Both
-- belong to the ORDER as the buyer gave them: the consent is evidence of what that person agreed to at
-- that moment, so it is stored with the booking rather than inferred later from a mutable contact.
--
-- sms_opt_in defaults to FALSE, not TRUE: an absent tick is not permission, and a default of true would
-- turn every imported or backfilled row into a consent nobody gave.
ALTER TABLE booking_orders ADD COLUMN IF NOT EXISTS phone text;
ALTER TABLE booking_orders ADD COLUMN IF NOT EXISTS sms_opt_in boolean NOT NULL DEFAULT false;
