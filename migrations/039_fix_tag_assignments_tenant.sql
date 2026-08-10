-- 039_fix_tag_assignments_tenant.sql
-- Fix C2: Add tenant_id to tag_assignments for tenant isolation
-- Also add unique constraint including tenant_id

ALTER TABLE tag_assignments ADD COLUMN tenant_id UUID REFERENCES tenants(id);

-- Drop the old unique constraint if it exists
ALTER TABLE tag_assignments DROP CONSTRAINT IF EXISTS tag_assignments_tag_id_entity_type_entity_id_key;

-- Add new unique constraint with tenant_id
ALTER TABLE tag_assignments ADD UNIQUE(tag_id, entity_type, entity_id, tenant_id);
