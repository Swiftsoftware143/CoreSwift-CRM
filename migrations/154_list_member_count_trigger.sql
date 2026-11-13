-- 154 — keep lists.member_count honest for every writer
--
-- `lists.member_count` is displayed on the Lists screen, but nothing in the application ever
-- writes it: the external contact endpoint (POST /api/external/contacts with a list_id) inserts
-- the `list_members` row and leaves the count at its default 0, so a list that visibly holds
-- contacts reports "0 members". The same is true of the console's own add-member and
-- remove-member paths — they touch `list_members` only. The handful of counts that looked right
-- were seeded once, not maintained.
--
-- A trigger rather than a Rust fix: the table has several writers (console, external API, list
-- evaluator, automation actions, n8n), and a per-call-site fix would drift again the moment a
-- new writer appears. The trigger recomputes from `list_members`, so it is idempotent and heals
-- any count that was already wrong.

CREATE OR REPLACE FUNCTION sync_list_member_count() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE lists
           SET member_count = (SELECT count(*) FROM list_members WHERE list_id = NEW.list_id)
         WHERE id = NEW.list_id;
        RETURN NEW;
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE lists
           SET member_count = (SELECT count(*) FROM list_members WHERE list_id = OLD.list_id)
         WHERE id = OLD.list_id;
        RETURN OLD;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_list_member_count ON list_members;
CREATE TRIGGER trg_list_member_count
    AFTER INSERT OR DELETE ON list_members
    FOR EACH ROW EXECUTE FUNCTION sync_list_member_count();

-- Backfill every count that drifted before the trigger existed.
UPDATE lists l
   SET member_count = (SELECT count(*) FROM list_members lm WHERE lm.list_id = l.id);
