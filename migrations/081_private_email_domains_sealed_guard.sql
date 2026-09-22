-- t_45772522: the two other credential columns of the private-email module get the same DB guard.
--
-- Their writers moved onto `secret_box::seal` in this card (they used to store the bare AES-GCM
-- body with no `enc:v1:` prefix), so the stored form is one format again. This arms the DATABASE so
-- a future handler that forgets to seal fails loudly instead of writing a live credential in the
-- clear. Same shape as 075/077/078/079.
--
-- NOT VALID on purpose: rows written before the fix are exempt, every NEW write is checked
-- immediately, and the boot audit (secret_box::audit_plaintext_secrets) keeps reporting this column
-- on every start. No semicolon anywhere in these comments - the in-app runner executes the whole file.
ALTER TABLE private_email_domains ADD CONSTRAINT private_email_domains_mailgun_key_sealed CHECK (mailgun_api_key = '' OR mailgun_api_key LIKE 'enc:v1:%') NOT VALID;
ALTER TABLE private_email_domains ADD CONSTRAINT private_email_domains_smtp_password_sealed CHECK (smtp_password_encrypted IS NULL OR smtp_password_encrypted = '' OR smtp_password_encrypted LIKE 'enc:v1:%') NOT VALID;
