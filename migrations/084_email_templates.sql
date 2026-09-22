-- 084_email_templates.sql - the platform transactional-email registry, and the row that feeds it.
--
-- t_e0c475ea. `email_templates` has been LIVE since 2026-08-08 but NO migration in this repo ever
-- created it (grep -rli email_templates migrations/ = 0 files), while the boot path runs
-- sqlx::migrate!. A fresh database therefore came up with no such table and every transactional mail
-- fell back to the inline bodies in src/email.rs, while the live database had drifted to a shape the
-- code did not know: it carries tenant_id and body_text and has NO is_html column - and
-- email.rs::send_template_email selected that missing column on EVERY send, swallowed the error with
-- `.ok().flatten()` and quietly used the inline fallback, so the registry never contributed a byte to
-- an email. The query now names the live columns and reports its errors, and the shape lives here.
--
-- Idempotent against the live database, where the table already exists in exactly this shape: CREATE
-- TABLE IF NOT EXISTS, CREATE INDEX IF NOT EXISTS, ON CONFLICT DO NOTHING on the seed, and an UPDATE
-- guarded by the legacy text it repairs. The seeded id is the id already live, so no row is lost and
-- the inventory is unchanged by this file.
--
-- Placeholders are DOUBLE braces: the one contract render_template() implements and the one
-- /api/email-templates/merge-fields publishes. The row as seeded out-of-band used SINGLE braces
-- ({app_name}, {name}, {email}, {password} and {login_url}, which is not even a merge field), which
-- render literally - mailed as-is it would have said "Welcome to {app_name}!" to every new customer.
-- src/email.rs carries a unit test that renders THIS text and fails if either side drifts, and a
-- leftover placeholder is now logged by the send path instead of being mailed.
--
-- No semicolon appears inside these comments - the in-app runner executes the whole file.

CREATE TABLE IF NOT EXISTS email_templates (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    template_type text NOT NULL,
    name          text NOT NULL,
    subject       text NOT NULL,
    body          text,
    html_body     text,
    is_default    boolean DEFAULT false,
    aid           uuid,
    created_at    timestamp with time zone DEFAULT now(),
    updated_at    timestamp with time zone DEFAULT now(),
    tenant_id     uuid,
    body_text     text,
    CONSTRAINT email_templates_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id)
);

-- "one default per (template_type, aid)": the registry handler maps a violation to 409.
CREATE UNIQUE INDEX IF NOT EXISTS idx_email_templates_unique
    ON email_templates (template_type, COALESCE(aid, '00000000-0000-0000-0000-000000000000'::uuid), is_default)
    WHERE aid IS NULL AND is_default = true;

-- The platform's own welcome email, under the id the live row already carries. is_default = true with
-- aid/tenant_id NULL is what email.rs resolves for every tenant that has no row of its own.
INSERT INTO email_templates (id, template_type, name, subject, body, html_body, is_default)
VALUES ('32e0cbdd-45a7-469a-995d-ed0daa9499fa', 'welcome', 'Default Welcome Email',
        'Welcome to {{app_name}}!',
        'Welcome to {{app_name}}, {{name}}!

Your account has been created successfully.

Your Login Credentials:
Email: {{email}}
Password: {{password}}

Login: {{app_url}}/login

Best,
The {{app_name}} Team',
        '<h2>Welcome to {{app_name}}, {{name}}!</h2><p>Your account has been created successfully.</p><p><strong>Login Credentials:</strong><br>Email: {{email}}<br>Password: {{password}}</p><p><a href="{{app_url}}/login">Log in here</a></p><p>Best,<br>The {{app_name}} Team</p>',
        true)
ON CONFLICT (id) DO NOTHING;

-- The live row predates this file and is the one statement here that is not a no-op: it carries the
-- single-brace placeholders, which do not render. The guard is that legacy subject, so this repairs
-- the shipped row exactly once and a later curated edit of the template is never overwritten.
UPDATE email_templates
   SET subject = 'Welcome to {{app_name}}!',
       body = 'Welcome to {{app_name}}, {{name}}!

Your account has been created successfully.

Your Login Credentials:
Email: {{email}}
Password: {{password}}

Login: {{app_url}}/login

Best,
The {{app_name}} Team',
       html_body = '<h2>Welcome to {{app_name}}, {{name}}!</h2><p>Your account has been created successfully.</p><p><strong>Login Credentials:</strong><br>Email: {{email}}<br>Password: {{password}}</p><p><a href="{{app_url}}/login">Log in here</a></p><p>Best,<br>The {{app_name}} Team</p>',
       updated_at = now()
 WHERE id = '32e0cbdd-45a7-469a-995d-ed0daa9499fa'
   AND subject = 'Welcome to {app_name}!';
