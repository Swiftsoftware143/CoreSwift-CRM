-- 035_password_changed_at.sql
-- Add password_changed_at column to users table for JWT blacklisting
-- Tokens issued before this timestamp are invalidated on password change

ALTER TABLE users ADD COLUMN IF NOT EXISTS password_changed_at TIMESTAMPTZ;
