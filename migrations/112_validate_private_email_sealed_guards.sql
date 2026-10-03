-- t_8851da4d: arm the stronger half of the three private-email sealed guards.
--
-- 080/081 ADDed these CHECK guards NOT VALID and carried no self-heal VALIDATE block, and 092
-- validated the four OTHER sealed guards (provider_keys, integration_targets, webhook_endpoints,
-- booking_calendars) but not these three. Live read convalidated = f for all three, so the fleet
-- guard-constraint-validity-sweep alerted on every tick: new writes were already rejected, but
-- 'every existing row is compliant' had never armed.
--
-- This is the canonical self-heal block of /opt/swift/fleet/templates/guard-constraint-not-valid.sql
-- (fleet convention §1-§4, decision t_c9b09cc1), shipped WITHOUT the DROP/ADD pair because the
-- constraints already exist and 080/081 must stay untouched - CoreSwift-CRM is an applies-once
-- (sqlx::migrate!) app and an applied file is checksummed, so a new higher-numbered file is the
-- only legal carrier. The block skips a constraint that is already validated and swallows
-- check_violation as a WARNING, so it can never fail a boot and can never leave a guard absent.
-- All three tables are EMPTY live, so the scan succeeds immediately and the stronger guarantee
-- becomes true for good, and a fresh install lands them validated too.
-- No semicolon anywhere in these comments - a naive in-app runner may execute the whole file.

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'private_email_api_keys_sealed'
          AND conrelid = 'private_email_api_keys'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE private_email_api_keys VALIDATE CONSTRAINT private_email_api_keys_sealed;
            RAISE NOTICE 'private_email_api_keys.private_email_api_keys_sealed validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'private_email_api_keys.private_email_api_keys_sealed still NOT VALID: pre-existing rows violate the guard (backfill them, then re-run this file). New writes are still rejected';
        END;
    END IF;
END
$guard$;

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'private_email_domains_mailgun_key_sealed'
          AND conrelid = 'private_email_domains'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE private_email_domains VALIDATE CONSTRAINT private_email_domains_mailgun_key_sealed;
            RAISE NOTICE 'private_email_domains.private_email_domains_mailgun_key_sealed validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'private_email_domains.private_email_domains_mailgun_key_sealed still NOT VALID: pre-existing rows violate the guard (backfill them, then re-run this file). New writes are still rejected';
        END;
    END IF;
END
$guard$;

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'private_email_domains_smtp_password_sealed'
          AND conrelid = 'private_email_domains'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE private_email_domains VALIDATE CONSTRAINT private_email_domains_smtp_password_sealed;
            RAISE NOTICE 'private_email_domains.private_email_domains_smtp_password_sealed validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'private_email_domains.private_email_domains_smtp_password_sealed still NOT VALID: pre-existing rows violate the guard (backfill them, then re-run this file). New writes are still rejected';
        END;
    END IF;
END
$guard$;
