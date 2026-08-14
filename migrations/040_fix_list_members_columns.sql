-- 040_fix_list_members_columns.sql
-- Fix C3: Add missing columns to list_members

ALTER TABLE list_members ADD COLUMN IF NOT EXISTS tenant_id UUID REFERENCES tenants(id);
ALTER TABLE list_members ADD COLUMN IF NOT EXISTS added_manually BOOLEAN DEFAULT false;
