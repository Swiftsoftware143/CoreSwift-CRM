-- 116_tenant_email_branding.sql
-- David 2026-10-08 (kanban t_feab8aff, porting t_c06a32eb from FunnelSwift): every transactional
-- email this app sends should carry the TENANT's own branding — a logo and a brand display name —
-- settable by the tenant in their own console, so the mail a business's users receive looks like
-- that business's mail.
--
-- Two pieces of storage, and only ONE of them is new:
--
--  1. `tenants.settings` key `email_branding` = {"brand_name", "brand_color", "logo_url"}. No DDL:
--     `tenants.settings` is the workspace's own jsonb document (read and written by
--     GET/PATCH /api/account/:id/settings — the route the console's Settings screen already uses),
--     and a jsonb document is exactly the shape it already holds. A workspace with no
--     `email_branding` key is simply unbranded and gets byte-identical mail to before this feature.
--
--  2. `tenant_logos` — the logo's BYTES. Same storage decision as FunnelSwift's user_avatars
--     (098) and tenant_logos (099): `docker inspect coreswiftcrm` binds only the release binary and
--     `migrations/`, so a file written at run time lives inside the container and dies with the next
--     `docker restart`, and no host webroot can serve it. The bytes are kept in the database and
--     streamed back by `GET /api/branding/logo/:tenant_id` (PUBLIC — a mail client sends no
--     credential).
--
-- One row per tenant: the logo is the workspace's, not per-user and not per-app, and the upload
-- path upserts it. Deleting the tenant must take the logo with it, which the FK's ON DELETE CASCADE
-- covers; the primary key is the only lookup path, so no extra index is needed.
CREATE TABLE IF NOT EXISTS tenant_logos (
    tenant_id    uuid         PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    content_type varchar(100) NOT NULL,
    bytes        bytea        NOT NULL,
    updated_at   timestamp    NOT NULL DEFAULT NOW()
);
