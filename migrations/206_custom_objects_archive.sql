-- 206 — Custom Objects: an object can be ARCHIVED, and a record can be PURGED.
--
-- The pilot (kanban t_575fe57f §6a) found that an object, once created, could only be removed with
-- SQL on the live database. Archive is that exit door and it is the ONLY one: nothing in this slice
-- deletes an object row, so a mistaken slug or a prototype shape stops being permanent clutter while
-- its records stay where they are.
--
-- archived_at on the OBJECT is what makes it disappear: `custom_objects::store::object_by_slug` and
-- `list_objects` read live rows only, so describe/records/audit/fields all answer 404 for an archived
-- object, and its rows are reachable by id from the database (support) alone. The FIELDS carry their
-- own archived_at (from 204) and are archived with the object.
ALTER TABLE custom_objects ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;

-- Idempotent archive: one lookup by (tenant_id, slug) that finds the live row.
CREATE INDEX IF NOT EXISTS idx_custom_objects_live
    ON custom_objects (tenant_id, slug)
    WHERE archived_at IS NULL;

-- A slug is unique among LIVE objects, not among all objects that ever existed. Without this, a
-- customer who archived a typo'd or abandoned shape could never create that slug again, and the
-- records of the archived object stay attached to it by object_id, so there is nothing to collide
-- with. The constraint is replaced by the partial unique index (same guarantee for live rows).
ALTER TABLE custom_objects DROP CONSTRAINT IF EXISTS custom_objects_tenant_slug;
DROP INDEX IF EXISTS custom_objects_tenant_slug;
CREATE UNIQUE INDEX IF NOT EXISTS custom_objects_tenant_slug_live
    ON custom_objects (tenant_id, slug)
    WHERE archived_at IS NULL;
