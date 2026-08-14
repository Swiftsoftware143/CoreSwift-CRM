-- 043_fix_tags_is_active.sql
-- Fix L4: Add is_active column to tags table

ALTER TABLE tags ADD COLUMN IF NOT EXISTS is_active BOOLEAN DEFAULT true;
