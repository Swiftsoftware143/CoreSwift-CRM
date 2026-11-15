-- The portfolio welcome mail, which is the ONE place an API key prefix belongs.
--
-- David's decision (2026-11-15), on the CEO's recommendation (option 2): the CUSTOMER welcome mail stays exactly
-- as it is — login address and password only. A free customer who only ever uses the CRM does not need a key
-- fragment in their inbox, and the fewer places a credential fragment appears the better. Portfolio accounts are
-- the ones wired into integrations, so that is where the prefix earns its place.
--
-- Registered as a global default in the same registry the customer welcome lives in, so it is editable from the
-- panel without a redeploy. Carries the login, the password and the key PREFIX — never the key.
INSERT INTO email_templates (template_type, name, subject, body, is_default)
SELECT 'portfolio_welcome',
       'Portfolio account welcome',
       'Your {{app_name}} account — login inside',
       E'Welcome to {{app_name}}, {{name}}!\n\nYour portfolio account is ready.\n\nYour Login Credentials:\nEmail: {{email}}\nPassword: {{password}}\n\nAPI key: {{key_prefix}}…\nThe full key is in your account under Settings → API keys. The prefix above is only so you can tell which key this is; it is never the whole key and never enough to use.\n\nSign in: {{app_url}}/login\n\nThis password is the same on every app in the portfolio, so one login works across them.\n\nBest,\nThe {{app_name}} Team',
       true
 WHERE NOT EXISTS (SELECT 1 FROM email_templates WHERE template_type = 'portfolio_welcome');
