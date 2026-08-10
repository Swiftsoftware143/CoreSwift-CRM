-- 041_fix_pipeline_stages_position.sql
-- Fix C4: Rename sort_order → position to match code expectations

ALTER TABLE pipeline_stages RENAME COLUMN sort_order TO position;
