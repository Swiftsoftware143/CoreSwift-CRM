-- A booking can add the customer to a LIST, not only attach a TAG.
--
-- David, 2026-11-20: "appointments being booked ... should connect with a list or tag or both ... and
-- those tags [and] list[s] are connected to automation. This is separate from the reminders."
--
-- The tag half already existed (`booking_tag_rules`: per calendar, per stage, add or remove a tag by
-- name). The list half did not, so an owner could tag a booking but could not put the booker into a
-- list — and it is the list, or the tag, that an automation rule watches.
--
-- Same shape as booking_tag_rules on purpose: one row per (calendar, list, stage), and a stage is the
-- question the rule answers — "who is confirmed", "who cancelled" — not a schedule. Reminders are a
-- different feature and stay in booking_reminders.
--
-- Additive only.

CREATE TABLE IF NOT EXISTS booking_list_rules (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    calendar_id uuid NOT NULL REFERENCES booking_calendars(id) ON DELETE CASCADE,
    list_id     uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    -- 'confirm' = the booking was confirmed, 'cancel' = it was cancelled.
    stage       text NOT NULL CHECK (stage IN ('confirm', 'cancel')),
    -- 'add' puts the booker in the list, 'remove' takes them out (the owner's choice of what the
    -- membership MEANS: "has booked" or "is booked right now").
    mode        text NOT NULL DEFAULT 'add' CHECK (mode IN ('add', 'remove')),
    is_active   boolean NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (calendar_id, list_id, stage)
);

CREATE INDEX IF NOT EXISTS idx_booking_list_rules_calendar
    ON booking_list_rules (calendar_id, stage) WHERE is_active;
