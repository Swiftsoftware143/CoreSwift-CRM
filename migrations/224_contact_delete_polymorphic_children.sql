-- 224 — a contact delete can no longer leave tag assignments (or list members) behind
--
-- `tag_assignments.entity_id` is polymorphic (`entity_type` + `entity_id`) and therefore has no FK to
-- `contacts`; the only FKs on that table are `tag_id` and `tenant_id`, both ON DELETE CASCADE. Deleting a
-- contact therefore left its assignments in place forever, and every tag count that read
-- `tag_assignments` without joining `contacts` reported those leftovers as members: on 2026-11-26 the
-- "Quote Request" tag showed 90 members for 1 real contact, and 139 of the 157 assignment rows pointed at
-- a contact id that did not exist (kanban t_7a54cc6a).
--
-- `list_members.contact_id` does cascade, but it is retired here as well: the rule this migration encodes
-- is "no row that names a contact outlives the contact", and it must hold for EVERY delete path — the
-- console handler, an internal API, a probe teardown, an operator running SQL by hand. A BEFORE DELETE
-- trigger is the only place that covers all of them.
--
-- The deletes are no-ops when the caller already removed the rows (the console handler now does so in the
-- same transaction as the contact), so this is idempotent and cannot double-count anything.
--
-- NOT swept here: the leftovers already in the table. The card requires a dry run and a before/after on
-- the live app, so the backlog is retired by a guarded sweep, not by a boot-time side effect.

CREATE OR REPLACE FUNCTION contacts_delete_polymorphic_children() RETURNS trigger AS $$
BEGIN
    DELETE FROM tag_assignments
     WHERE entity_type = 'contact' AND entity_id = OLD.id;
    DELETE FROM list_members
     WHERE contact_id = OLD.id;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_contacts_delete_polymorphic_children ON contacts;
CREATE TRIGGER trg_contacts_delete_polymorphic_children
    BEFORE DELETE ON contacts
    FOR EACH ROW EXECUTE FUNCTION contacts_delete_polymorphic_children();
