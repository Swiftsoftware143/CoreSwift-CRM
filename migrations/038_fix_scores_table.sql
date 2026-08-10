-- 038_fix_scores_table.sql
-- Fix C1: Rename contact_scores → scores to match code queries
ALTER TABLE IF EXISTS contact_scores RENAME TO scores;
-- NOTE: If the table is named "scores" already, this is a no-op.
-- If neither table exists, scoring queries will fail at runtime.
