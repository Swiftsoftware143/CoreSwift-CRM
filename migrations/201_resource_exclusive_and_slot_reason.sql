-- Date exclusivity: a position can block every other position on a date it is sold.
--
-- Why this exists (SwiftSoftware, 2026-11-23): four positions are sold per issue (Email TOP / MIDDLE /
-- FOOTER / Sponsored Post) and a "Full Takeover" sells all four in one issue, exclusive. With every
-- position independent, the takeover could be sold while all four individual slots stayed on sale —
-- the same inventory sold twice. This flag makes the block real in both directions: an exclusive
-- position blocks the others on a date it is booked, and a booked individual position blocks the
-- exclusive one on that date.

ALTER TABLE booking_resources
    ADD COLUMN IF NOT EXISTS exclusive boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN booking_resources.exclusive IS
    'When true, a booking on this position closes every other position on the same date, and the '
    'reverse: any other position booked on a date closes this one for that date.';

-- A closed slot that was closed BY an exclusivity rule says so, so an operator reading a date can
-- tell "we pulled it" from "it was blocked by the takeover sold on that date".
ALTER TABLE booking_slots
    ADD COLUMN IF NOT EXISTS closed_reason text;

COMMENT ON COLUMN booking_slots.closed_reason IS
    'Why a slot is closed, e.g. ''exclusive:Full Takeover''. NULL means closed by hand or by default.';
