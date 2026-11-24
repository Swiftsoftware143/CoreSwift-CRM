-- 208 — Custom Objects: a COMBINATION of fields can be unique, and a write can be replayed.
--
-- Measured 2026-11-24 (ZaarHub CEO, bus seq 2713): a field could be `unique_value` on its own, but
-- there was no composite key and no INSERT ... ON CONFLICT anywhere in the module, so a producer
-- that replayed "write this ad's fulfilment row" (booking_ref + position) inserted a second row every
-- time. A booking with two positions could end up with four live rows after one retry.
--
-- `unique_together` names the field keys whose COMBINATION must be unique among LIVE records. It is
-- validated against live fields by the ONE writer (`custom_objects::store::set_unique_together`) and
-- refused while duplicates already exist — with the colliding keys and row ids named, so the caller
-- knows what to fix instead of guessing.
--
-- ENFORCEMENT is `custom_records.uniq_key`, a text column the writer fills from the object's
-- `unique_together` (a canonical JSON array of the values). Why a materialised column and not an
-- expression index over `data->>k1, data->>k2, ...`: the key set is DATA, not schema — an expression
-- index would mean the app builds DDL out of field names at run time (`CREATE UNIQUE INDEX ... ON
-- custom_records ((data->>'booking_ref'), ...)`), which is exactly the string-built SQL the deploy
-- gate refuses and an injection surface with no database-side protection. One fixed index over one
-- fixed column enforces ANY key set, and `ON CONFLICT (object_id, uniq_key)` is a literal statement.
--
-- The index is PARTIAL on purpose:
--   * `deleted_at IS NULL` — a soft-deleted row must not block the same key from being written again.
--   * `uniq_key IS NOT NULL` — a record that simply does not carry the key values is not keyed at
--     all, and PostgreSQL already allows many NULLs in a unique index.
ALTER TABLE custom_objects
    ADD COLUMN IF NOT EXISTS unique_together JSONB NOT NULL DEFAULT '[]'::jsonb;

-- The column is a JSON array of field keys, never an object or a bare string: the writer binds it,
-- but a hand-written UPDATE must not be able to store a shape the writer cannot read.
ALTER TABLE custom_objects
    ADD CONSTRAINT custom_objects_unique_together_is_array
    CHECK (jsonb_typeof(unique_together) = 'array');

-- The ONE constraint behind every replay-safe write. See the header for why it is a column and why
-- it is partial.
ALTER TABLE custom_records ADD COLUMN IF NOT EXISTS uniq_key TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS custom_records_uniq_key_live
    ON custom_records (object_id, uniq_key)
    WHERE deleted_at IS NULL AND uniq_key IS NOT NULL;
