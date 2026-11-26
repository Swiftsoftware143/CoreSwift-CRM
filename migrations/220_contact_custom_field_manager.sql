-- CONTACT CUSTOM FIELDS GET A MANAGER (2026-11-25).
--
-- The tables have existed for a while (contact_custom_fields has 33 rows on ZaarHub) and nothing manages
-- them: the deals twin has list/create/update/delete routes, contacts had none, so "a field created at a
-- capture becomes a real contact field" had nowhere to be created.
--
-- `position` and `options` are what the deal side already carries: an explicit order (David: name, title,
-- phone, email, then the custom ones) and the choices for a select field.
ALTER TABLE contact_custom_fields
    ADD COLUMN IF NOT EXISTS position integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS options jsonb NOT NULL DEFAULT '[]'::jsonb;

COMMENT ON COLUMN contact_custom_fields.position IS
    'Order on the contact card, after the built-in fields. Ties break by label.';
COMMENT ON COLUMN contact_custom_fields.options IS
    'Allowed choices for a select field; ignored for every other type.';
