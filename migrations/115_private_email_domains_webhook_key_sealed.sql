-- t_3df8c350: the FOURTH credential column of the private-email module gets the sealed-or-empty guard.
--
-- t_8851da4d armed private_email_api_keys.api_key_encrypted, private_email_domains.mailgun_api_key and
-- private_email_domains.smtp_password_encrypted (080/081 ADD NOT VALID, 112 VALIDATE) and called them
-- "the three private-email sealed guards" -- but private_email_domains carries a FOURTH credential
-- column, webhook_signing_key_encrypted (056), which had no constraint at all.
--
-- Measured live 2026-10-06: the nightly tenancy canary (fleet/coreswift-tenancy-canary.py) plants its
-- per-run marker into every plantable text column, and its hint machinery substitutes the BLANK arm
-- (empty string) for any column that carries this check -- which is why the three guarded columns are
-- stored empty by the probe. The unguarded column is the one that receives the raw marker, so the app's
-- own boot audit (secret_box::audit_plaintext_secrets) reported "PLAINTEXT SECRETS AT REST" with
-- count=1 on every start, a false alarm that trains a reader to ignore the one control that would catch
-- a real unsealed credential.
--
-- The column has no writer in the app (only reads, src/private_email/send_handler.rs), so arming the
-- guard changes no legal write. Same shape as 075/077/078/079/080/081, canonical self-heal block from
-- fleet/templates/guard-constraint-not-valid.sql. NOT VALID on ADD so the boot stays cheap, then the
-- VALIDATE block claims the stronger guarantee as soon as every existing row is compliant.
--
-- No semicolon anywhere in these comments -- the in-app runner executes the whole file.
ALTER TABLE private_email_domains DROP CONSTRAINT IF EXISTS private_email_domains_webhook_key_sealed;

ALTER TABLE private_email_domains ADD CONSTRAINT private_email_domains_webhook_key_sealed CHECK (webhook_signing_key_encrypted IS NULL OR webhook_signing_key_encrypted = '' OR webhook_signing_key_encrypted LIKE 'enc:v1:%') NOT VALID;

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'private_email_domains_webhook_key_sealed'
          AND conrelid = 'private_email_domains'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE private_email_domains VALIDATE CONSTRAINT private_email_domains_webhook_key_sealed;
            RAISE NOTICE 'private_email_domains.private_email_domains_webhook_key_sealed validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'private_email_domains.private_email_domains_webhook_key_sealed still NOT VALID: pre-existing rows violate the guard (backfill them, then re-run this file); new writes are still rejected';
        END;
    END IF;
END
$guard$;
