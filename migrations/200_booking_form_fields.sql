-- WHAT THE BOOKING FORM ASKS, AND WHICH ANSWERS ARE MANDATORY (David, 2026-11-23).
--
-- Today a booking captures the booker's name, email and phone and nothing else. A newsletter selling a
-- sponsorship needs the COMPANY too, and the owner must be able to say which of those are mandatory.
--
-- The three the engine already holds (name, email, phone) stay built in — they are how a booking is
-- delivered and how the person is found again, so they cannot be removed, only made mandatory. Any
-- other question is a row here, and the answers are stored on the order.
CREATE TABLE IF NOT EXISTS booking_form_fields (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    calendar_id  uuid NOT NULL REFERENCES booking_calendars(id) ON DELETE CASCADE,
    key          text NOT NULL,
    label        text NOT NULL,
    kind         text NOT NULL DEFAULT 'text',
    required     boolean NOT NULL DEFAULT false,
    placeholder  text,
    help         text,
    sort_order   integer NOT NULL DEFAULT 0,
    is_active    boolean NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS booking_form_fields_key
    ON booking_form_fields (calendar_id, lower(key));
CREATE INDEX IF NOT EXISTS booking_form_fields_calendar
    ON booking_form_fields (calendar_id, sort_order);

COMMENT ON TABLE booking_form_fields IS
    'Extra questions on a booking form, and whether the answer is required. Name, email and phone are built in and always captured.';

-- The answers themselves, beside the order they belong to. JSON rather than a table per question: the
-- answers are read with the order and never queried on their own.
ALTER TABLE booking_orders
    ADD COLUMN IF NOT EXISTS answers jsonb NOT NULL DEFAULT '{}'::jsonb;
