-- 140_private_email_folder_parent_width.sql
--
-- A folder a customer made can hold subfolders. The rail's "Create subfolder" sends the parent
-- folder's PATH ('archive/client files') and create_folder stores that path in `parent`, so the
-- nesting chain a message path already carries is visible in the rail.
--
-- The column was sized for a system folder name (varchar(16)), so creating a subfolder inside a made
-- folder failed with `value too long for type character varying(16)` and answered 500 — measured on
-- live 2026-11-11 (kanban t_d43ee278): POST /api/private-email/folders
-- {"parent":"archive/Client Files","name":"Sub"}.
--
-- 160 is the width `path` already has, and `parent` is always a prefix of `path`, so it can never be
-- longer than what the path column already accepts.

ALTER TABLE private_email_folders ALTER COLUMN parent TYPE varchar(160);
