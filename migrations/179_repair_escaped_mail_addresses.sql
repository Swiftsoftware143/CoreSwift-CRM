-- Addresses that arrived carrying a JSON escape instead of the character it stands for.
--
-- NUMBER MOVED 177 -> 179 (2026-11-16): this file was committed as 177 alongside
-- `177_portfolio_flag_from_registry.sql`, which had ALREADY been applied (the live ledger's version
-- 177 is "portfolio flag from registry", success=true). Two files sharing one version make the
-- migrator refuse the version, and the pair could never be deployed — the running artifact embedded
-- neither this file nor 178. This file was NEVER applied (no ledger row carries its description), so
-- moving its number is safe: nothing has to be un-applied, the work simply runs at boot now.

-- Measured on the live box (2026-11-15): private_email_messages.address held values like
-- `u003esupport@giraudycapital.com`, where `\u003e` (the escaped `>`) leaked into the address stored for the
-- message. It breaks every per-address read: the address filter cannot match it, a per-inbox count misses it, and
-- the row looks like mail for a mailbox that does not exist.
--
-- The escape can only ever be a leading artefact of the recipient header, so this repairs the leading run and
-- touches nothing else. Idempotent: re-running finds nothing.
UPDATE private_email_messages
   SET address = regexp_replace(address, '^u003e+', '')
 WHERE address ~ '^u003e+';

-- The same artefact on the sender, which is what a customer-facing list shows.
UPDATE private_email_messages
   SET from_address = regexp_replace(from_address, '^u003e+', '')
 WHERE from_address ~ '^u003e+';
