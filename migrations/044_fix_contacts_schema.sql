-- 044_fix_contacts_schema.sql
-- Fix M5/M6: Add columns expected by code model that are missing from migration 003
-- Migration 003 has: company (VARCHAR), job_title (VARCHAR), city, state, country
-- Code expects: company_id (UUID), title, gender, address_line1, address_line2, postal_code, notes
-- This migration adds the missing columns without dropping existing ones

ALTER TABLE contacts ADD COLUMN IF NOT EXISTS notes TEXT;
ALTER TABLE contacts ADD COLUMN IF NOT EXISTS gender VARCHAR(50);
ALTER TABLE contacts ADD COLUMN IF NOT EXISTS address_line1 VARCHAR(255);
ALTER TABLE contacts ADD COLUMN IF NOT EXISTS address_line2 VARCHAR(255);
ALTER TABLE contacts ADD COLUMN IF NOT EXISTS postal_code VARCHAR(20);
ALTER TABLE contacts ADD COLUMN IF NOT EXISTS company_id UUID;
ALTER TABLE contacts ADD COLUMN IF NOT EXISTS title VARCHAR(255);
