-- 118_user_profile_company_and_avatars.sql
-- kanban t_87b857f8 — the fleet account/profile surface for CoreSwift-CRM.
--
-- The workspace Profile screen needs two fields the `users` row did not carry (company,
-- username) and a per-user profile PICTURE. `users.avatar_url` already exists (baseline schema)
-- and was never written by any code; it now holds the user-scoped serve URL.
--
-- The picture's BYTES live in the database, for one measured reason shared with `tenant_logos`
-- (migration 116): this container runs the app from the image-built `/app/crm-swift` with no web
-- root bind-mounted (`docker inspect coreswiftcrm` -> no mounts), so a file written at run time
-- would die with the next `docker restart` and no nginx root could serve it either. See
-- `crate::image_store` for the one accept-store-serve path.
--
-- NOT reusing a contact path: in this CRM "avatar" already means a CONTACT's picture; this table
-- is keyed by `users.id` for the SIGNED-IN user's own picture only.

ALTER TABLE users ADD COLUMN IF NOT EXISTS username varchar(100);
ALTER TABLE users ADD COLUMN IF NOT EXISTS company  varchar(255);

CREATE TABLE IF NOT EXISTS user_avatars (
    user_id      uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    content_type varchar(100) NOT NULL,
    bytes        bytea        NOT NULL,
    updated_at   timestamptz  NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE user_avatars IS
    'Per-user profile picture (one row per user). Bytes in the DB; served anonymously by GET /api/avatars/:user_id, written by POST /api/profile/avatar. kanban t_87b857f8.';
