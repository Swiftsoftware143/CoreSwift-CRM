-- The artwork size a position takes, e.g. '600x150'.
--
-- Why (ZaarHub, 2026-11-24): a sponsor has to send artwork when they book, and the confirmation email
-- has to state the right spec. Hardcoding sizes per product name puts the spec in the mail template
-- instead of on the product, so a new position silently inherits the wrong figure. The size belongs to
-- the position and reaches the email as a merge field.

ALTER TABLE booking_resources
    ADD COLUMN IF NOT EXISTS size text;

COMMENT ON COLUMN booking_resources.size IS
    'Artwork spec for this position, e.g. 600x150. Empty means the confirmation email states no size.';
