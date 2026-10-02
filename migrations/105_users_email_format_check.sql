-- 105_users_email_format_check
--
-- WHY (kanban t_9252c512). `public.users.email` is an account's login identity AND the only address
-- its credentials/welcome mail can ever be delivered to. The column was `text NOT NULL UNIQUE` with
-- NO `CHECK`, and the public signup boundary (POST /api/v1/auth/register) only asked
-- `!email.contains('@')` and bound the value verbatim, so `a@b`, `bad@`, `@x.com` and `" a@b "` were
-- all stored as real logins no mail could ever reach. The class was measured live 2026-10-02 under
-- kanban t_4722a331 (0 malformed rows on live, so nothing to retire).
--
-- The application boundary now refuses that input with 422 `{"error":true,"message":"email: …"}`
-- before any SELECT/INSERT (src/security/email_addr.rs::normalize, used by all five measured writers
-- of users.email: auth::handlers::register, admin_actions::handlers::{handle_create_tenant_account,
-- cross_app_sync} and webhook::actions::route_action's tenants.create + users.invite arms). This
-- constraint is the store-level backstop for the writers nobody has written yet — the same "fix the
-- class, not the call site" posture the fleet applies elsewhere.
--
-- The pattern is deliberately LOOSER than the Rust validator so the database can never refuse a
-- value the application accepted: the application additionally rejects whitespace/control
-- characters, empty and dot-only local/domain parts, addresses over 254 characters, and a second
-- `@`. Everything this regex requires — a non-empty part, one `@`, a non-empty dotted domain — is
-- required by the application too. Plus-aliases (`a+b@x.com`), dotted locals (`a.b@x.com`) and IDN
-- domains (`user@münchen.de`) pass both. Every live row was checked against the pattern before this
-- file was written (7 rows, 0 violations).
--
-- sqlx::migrate! applies this file exactly once (checksummed in `_sqlx_migrations`), so unlike a
-- boot-time re-running runner there is no idempotency probe here — and this file must never be
-- edited after it is applied.

ALTER TABLE public.users
    ADD CONSTRAINT users_email_format_check
    CHECK (email ~ '^[^[:space:]@]+@[^[:space:]@]+\.[^[:space:]@]+$');
