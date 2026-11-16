-- Stop the escaped address at the door, on every path, including paths that do not exist yet.
--
-- Migration 177 repaired the addresses already stored. Repairing history is only half of it: the escape came from
-- the recipient header and could be re-introduced by any writer — two different modules insert into this table
-- today (the inbound path and the send path), and a third will be added eventually. A helper called by two
-- callers is one caller away from being forgotten, so the normalisation lives where every write passes: a trigger.
--
-- It strips only a LEADING run of `u003e` (what a leaked `\u003e` becomes once the backslash is gone) from the
-- address and the sender. Nothing else is touched, and the function is idempotent, so a clean address is stored
-- byte-for-byte as it arrived.

CREATE OR REPLACE FUNCTION normalise_mail_address() RETURNS trigger AS $$
BEGIN
    NEW.address      := regexp_replace(NEW.address, '^u003e+', '');
    NEW.from_address := regexp_replace(NEW.from_address, '^u003e+', '');
    RETURN NEW;
END $$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_normalise_mail_address ON private_email_messages;
CREATE TRIGGER trg_normalise_mail_address
    BEFORE INSERT OR UPDATE OF address, from_address ON private_email_messages
    FOR EACH ROW EXECUTE FUNCTION normalise_mail_address();
