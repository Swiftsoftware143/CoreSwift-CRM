-- Add payment_provider column to plans table for per-plan payment processor selection
ALTER TABLE plans ADD COLUMN IF NOT EXISTS payment_provider VARCHAR(64);
