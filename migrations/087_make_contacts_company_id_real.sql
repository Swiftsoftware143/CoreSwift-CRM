-- 087_make_contacts_company_id_real.sql
-- Give `contacts.company_id` the reference its writers already assume (card t_47698f73).
--
-- 044_fix_contacts_schema.sql added the column with the header line "Code expects: company_id
-- (UUID)" and no reference, and that is exactly what it stayed: measured live 2026-09-22, the
-- only objects on the column were `contacts_pkey` and `contacts_tenant_id_fkey` — no FK on
-- `company_id` — and all 6 of the 6 rows that ever carried a value (of 339) pointed at a uuid
-- that exists in no table at all:
--
--   select count(*) from contacts c left join companies co on co.id = c.company_id
--    where c.company_id is not null and co.id is null;   -> 6
--
-- The column is not dead, which is why it is repaired instead of dropped (the decision and the
-- consumer list are on the card): `src/webhook/actions.rs` puts `company_id` into the outbound
-- webhook payloads of the contact list/find actions, the same file scores `+10` when it is set,
-- and `POST /api/companies` exists, i.e. the link has real arity the moment a tenant creates a
-- company (there are 0 companies in the live DB today, so nothing has ever had a valid value).
--
-- `ON DELETE SET NULL`, not CASCADE: removing a company must unlink the people who worked there,
-- never delete them. Display precedence is untouched — the free-text `contacts.company` is what
-- the product renders (decision t_fbb30c16); this column is the machine-readable link.
--
-- Both statements were measured on live in a rolled-back transaction before this file was
-- written (ON_ERROR_STOP=1): UPDATE 6 cleared the dangling rows, the ALTER armed the FK, an
-- insert naming a nonexistent uuid was refused with 23503, a same-tenant pair stored and joined
-- 1, and deleting the company cleared `company_id` while leaving `tenant_id` intact.

-- 1. Clear the references that resolve to nothing, so the constraint can be armed on live data.
UPDATE contacts SET company_id = NULL
 WHERE company_id IS NOT NULL
   AND NOT EXISTS (SELECT 1 FROM companies co WHERE co.id = contacts.company_id);

-- 2. Arm the reference (idempotent: a database that already carries the constraint is left alone).
ALTER TABLE contacts DROP CONSTRAINT IF EXISTS contacts_company_id_fkey;
ALTER TABLE contacts ADD CONSTRAINT contacts_company_id_fkey
    FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE SET NULL;
