-- 045_cleanup_duplicate_constraints.sql
-- Drop old UNIQUE INDEX that conflicts with the new tenant-aware constraint.
-- The index idx_tag_assignments_unique on (tag_id, entity_type, entity_id)
-- duplicates the constraint tag_assignments_tag_id_entity_type_entity_id_tenant_id_key
-- and would cause insert failures when the same entity/tag combination exists
-- under different tenants.

DROP INDEX IF EXISTS idx_tag_assignments_unique;
