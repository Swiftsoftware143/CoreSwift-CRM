-- ONE TAG AT A TIME from a category that says so (David, 2026-11-26).
--
-- The industry-standard shape for a lead temperature is a MUTUALLY EXCLUSIVE group: a contact is Hot OR
-- Warm OR Cold, never two of them. Assigning "Hot" to somebody who already held "Warm" used to leave
-- both, which makes every count that reads the group wrong ("how many warm leads?" counts the hot ones
-- too), and makes scoring and routing disagree with what the screen shows.
--
-- Enforced in the DATABASE, not in one writer. The assign handler, the booking capture, the automation
-- actions, the dynamic-list evaluator and the MCP door all write tag_assignments; a rule that only the
-- first of those obeyed would be a suggestion.
--
-- Order matters and is the point: this is AFTER INSERT, so the new tag exists before the siblings are
-- removed. A contact is never left with no tag from the group - if the delete ran first and the insert
-- then failed, the person would be temperature-less.

ALTER TABLE tag_categories
    ADD COLUMN IF NOT EXISTS is_exclusive boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN tag_categories.is_exclusive IS
    'Only one tag from this category per entity: assigning a second one removes the first, after the new one is in place.';

CREATE OR REPLACE FUNCTION tag_assignments_keep_one_per_exclusive_category()
RETURNS trigger AS $$
DECLARE
    v_category uuid;
    v_exclusive boolean;
BEGIN
    SELECT t.category_id, c.is_exclusive
      INTO v_category, v_exclusive
      FROM tags t
      LEFT JOIN tag_categories c ON c.id = t.category_id
     WHERE t.id = NEW.tag_id;

    IF v_exclusive IS NOT TRUE OR v_category IS NULL THEN
        RETURN NEW;
    END IF;

    DELETE FROM tag_assignments ta
     USING tags t2
     WHERE ta.tag_id = t2.id
       AND t2.category_id = v_category
       AND ta.entity_type = NEW.entity_type
       AND ta.entity_id = NEW.entity_id
       AND ta.id <> NEW.id;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS tag_assignments_exclusive_trg ON tag_assignments;
CREATE TRIGGER tag_assignments_exclusive_trg
    AFTER INSERT ON tag_assignments
    FOR EACH ROW
    EXECUTE FUNCTION tag_assignments_keep_one_per_exclusive_category();
