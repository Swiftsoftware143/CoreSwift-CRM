-- 042_fix_automation_webhooks.sql
-- Fix C5: Add rate_limit_per_minute column for webhook throttling

ALTER TABLE automation_webhooks ADD COLUMN rate_limit_per_minute INTEGER DEFAULT 60;
