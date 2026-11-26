-- A booking can carry MANY tags and land on MANY lists (David, 2026-11-26).
--
-- `booking_calendars.sponsor_tag` was one nullable text column: one tag per calendar, and list
-- membership only happened if a dynamic rule happened to match that one tag. A real purchase is
-- several facts at once — the product bought, the city, "hot lead" — and the person belongs on more
-- than one list (a nurture list and a customer list are different lists for the same person).
--
-- Templates, not fixed names: {product}, {city}, {calendar}, {resource} are substituted at apply time
-- from what the buyer actually chose, so one calendar with fifty positions does not need fifty rows.

CREATE TABLE IF NOT EXISTS booking_calendar_capture_tags (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    calendar_id uuid NOT NULL REFERENCES booking_calendars(id) ON DELETE CASCADE,
    template    text NOT NULL CHECK (length(btrim(template)) > 0),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS booking_calendar_capture_tags_calendar_idx
    ON booking_calendar_capture_tags (calendar_id, created_at);

-- The same tag twice on one calendar is a mistake, not a feature.
CREATE UNIQUE INDEX IF NOT EXISTS booking_calendar_capture_tags_unique
    ON booking_calendar_capture_tags (calendar_id, lower(btrim(template)));

CREATE TABLE IF NOT EXISTS booking_calendar_capture_lists (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    calendar_id uuid NOT NULL REFERENCES booking_calendars(id) ON DELETE CASCADE,
    list_id     uuid NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (calendar_id, list_id)
);

CREATE INDEX IF NOT EXISTS booking_calendar_capture_lists_calendar_idx
    ON booking_calendar_capture_lists (calendar_id, created_at);

-- Backfill: every calendar that had a sponsor_tag keeps behaving exactly as it did, as a one-row list.
INSERT INTO booking_calendar_capture_tags (tenant_id, calendar_id, template)
SELECT c.tenant_id, c.id, btrim(c.sponsor_tag)
  FROM booking_calendars c
 WHERE c.sponsor_tag IS NOT NULL
   AND btrim(c.sponsor_tag) <> ''
ON CONFLICT DO NOTHING;

-- A CATEGORY, NOT A LIST OF NAMES (David, 2026-11-26): an owner groups tags into a category called
-- "Products" or "Services" once, and a calendar names the CATEGORY — so adding a new product tag later
-- needs no change to any calendar. Naming the category is the setting; its members are the data.
CREATE TABLE IF NOT EXISTS booking_calendar_capture_categories (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    calendar_id uuid NOT NULL REFERENCES booking_calendars(id) ON DELETE CASCADE,
    category_id uuid NOT NULL REFERENCES tag_categories(id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (calendar_id, category_id)
);
