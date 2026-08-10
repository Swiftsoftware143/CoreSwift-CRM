-- 040_fix_list_members_columns.sql
-- Fix C3: Add missing columns to list_members

ALTER TABLE list_members ADD COLUMN tenant_id UUID REFERENCES tenants(id);
ALTER TABLE list_members ADD COLUMN added_manually BOOLEAN DEFAULT false;
