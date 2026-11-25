-- Take the payment inside the booking page, and place it where the calendar type needs it.
--
-- Why (David, 2026-11-24): a paid checkout used to be a link OUT of the page. He wants the payment
-- form embedded in the page — their provider's embed code or a basic iframe — arriving as a small
-- transition rather than a jump to another site. And where it appears depends on WHAT is being sold:
--
--   * an appointment calendar (a consultant's time) is paid FOR THE BOOKING, so the form belongs in the
--     step that secures the time, before the buyer is told they are booked;
--   * an inventory/date_range calendar (a newsletter slot) is paid AFTERWARDS, so the form belongs on
--     the confirmation, still in the page, never a link away.
--
-- `payment_mode` records the owner's choice; NULL means the default above follows the calendar kind.
-- `payment_embed` holds what the owner pasted. Nothing from it is executed: only an https address is
-- read back out of it (see booking::service::payment_block), so a provider script embed degrades to its
-- iframe source rather than running foreign code on a customer-facing page.

ALTER TABLE booking_calendars
    ADD COLUMN IF NOT EXISTS payment_mode text;

ALTER TABLE booking_calendars
    ADD COLUMN IF NOT EXISTS payment_embed text;

DO $$
BEGIN
    ALTER TABLE booking_calendars
        ADD CONSTRAINT booking_calendars_payment_mode_chk
        CHECK (payment_mode IS NULL OR payment_mode IN ('before', 'after'));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

COMMENT ON COLUMN booking_calendars.payment_mode IS
    'when the buyer pays: "before" secures the booking (consultants), "after" is paid on the '
    'confirmation (newsletter slots). NULL follows the calendar kind.';

COMMENT ON COLUMN booking_calendars.payment_embed IS
    'The owner''s pasted checkout code or link. Only its https address is read back; nothing in it is '
    'executed on the customer-facing page.';
