-- 093_other_parent_fk_delete_actions.sql
-- t_ef53b9ff: the other half of the relation 091 armed — the NO ACTION edges that hang off parents
-- OTHER than `tenants`. 091 fixed the 12 `tenants` children; these 11 reference `users`, `contacts`,
-- `tags`, `message_templates`, `tracked_links` and `cs_messages`, so none of them was reachable by
-- that change and all 11 mean the same thing: deleting the PARENT raises 23503 while a child exists.
--
-- Measured live on `coreswift_crm` 2026-09-25 (HEAD 7f48e6e, evidence
-- /opt/swift/audits/cs-other-parent-fks-t_ef53b9ff/00-measure.txt and the census in
-- /opt/swift/audits/cs-tenant-delete-actions-t_d8d1a175/10-other-edges.txt):
--
--   parent            child               constraint                             col                NOT NULL  live child rows
--   contacts          cs_messages         cs_messages_recipient_id_fkey          recipient_id       no        0
--   contacts          events              events_contact_id_fkey                 contact_id         no        0
--   contacts          link_clicks         link_clicks_contact_id_fkey            contact_id         YES       0
--   cs_messages       cs_messages         cs_messages_parent_message_id_fkey     parent_message_id  no        0
--   message_templates notification_rules  notification_rules_template_id_fkey    template_id        no        0
--   tags              tracked_links       tracked_links_tag_id_fkey              tag_id             YES       0
--   tracked_links     link_clicks         link_clicks_tracked_link_id_fkey       tracked_link_id    YES       0
--   users             cs_messages         cs_messages_sender_id_fkey             sender_id          no        0
--   users             email_campaigns     email_campaigns_created_by_fkey        created_by         no        0
--   users             events              events_created_by_fkey                 created_by         no        0
--   users             notifications       notifications_user_id_fkey             user_id            no        3
--
-- Four of those parents have a live DELETE endpoint, so each of their edges is a 500 waiting for the
-- first child row:  src/contacts/handlers.rs:369 (contacts), src/tags/handlers.rs:227 +
-- src/tags/internal_handler.rs:268 (tags), src/tracked_links/handlers.rs:128 (tracked_links),
-- src/communications/handlers.rs:270 (message_templates), src/messages/handlers.rs:204 (cs_messages).
-- `users` has no delete surface in this repo (no `DELETE FROM users` anywhere in src/) — the exposure
-- is 3 live notifications rows that any operator or future user-delete feature would hit immediately.
--
-- ARM PER EDGE — taken from the code path that deletes the parent and the readers of the child, not
-- from taste. `ON DELETE SET NULL` is only available where the column is nullable, and it only keeps
-- a row meaningful where the child has a reader that does not depend on the pointer.
--
--   link_clicks.tracked_link_id  -> CASCADE   NOT NULL, so SET NULL is unavailable at this schema. A
--                                             click of a link that no longer exists is not data: it
--                                             is the link's metric, and every reader joins it to the
--                                             link (src/tracked_links/handlers.rs).
--   link_clicks.contact_id       -> CASCADE   NOT NULL (same). A click row is attributed to exactly
--                                             one contact; with the contact gone the row has no owner
--                                             and no reader left that can reach it.
--   tracked_links.tag_id         -> CASCADE   NOT NULL (same). The tag IS the link's category and the
--                                             only way a link is created or managed: create_tracked_link
--                                             refuses a tag that is missing or inactive in the tenant
--                                             (src/tracked_links/handlers.rs:24-38), and the list resolves
--                                             the tag name for every row (handlers.rs:95). A link left
--                                             behind by a deleted tag is an active redirect that the
--                                             API can never re-categorise — the one arm that must not
--                                             survive.
--   notification_rules.template_id -> SET NULL rule must keep firing without a template: `template_id`
--                                             is nullable (Option<Uuid> in src/notifications/handlers.rs:28),
--                                             the create/update paths bind it as NULL (create_rule
--                                             binds r.template_id, update_rule uses COALESCE($3, template_id)),
--                                             and the action is what drives the rule (`send_email` /
--                                             `in_app` — src/worker.rs inserts an in_app notification with
--                                             no template at all).
--   cs_messages.parent_message_id -> SET NULL  a reply is a message in its own right; cascading it away
--                                             because the message it answered was deleted destroys
--                                             content the user never asked to delete. The pointer is
--                                             the only thing that stops meaning anything, and the column
--                                             is nullable (src/messages/models.rs:30).
--   cs_messages.recipient_id     -> SET NULL   `SELECT * FROM cs_messages WHERE tenant_id = $1 …`
--                                             (src/messages/handlers.rs:41-115) — the message log is
--                                             scoped by the WORKSPACE, never filtered by recipient.
--   events.contact_id            -> SET NULL   event log, read by tenant + source / event_type /
--                                             created_at (src/events/handlers.rs:132-187) — never by
--                                             contact.
--   cs_messages.sender_id        -> SET NULL   same message-log readers as recipient_id: the row belongs
--                                             to the workspace; `sender_id` is `Option<Uuid>`
--                                             (src/messages/models.rs:18) and never a filter.
--   email_campaigns.created_by   -> SET NULL   `created_by: Option<Uuid>` (src/campaigns/models.rs:12),
--                                             read paths scope by tenant_id + status
--                                             (src/campaigns/handlers.rs:36-51) — the campaign is the
--                                             workspace's, its author is metadata.
--   events.created_by            -> SET NULL   same event-log readers; the author is metadata.
--   notifications.user_id        -> CASCADE    the one users edge that is meaningless without its author:
--                                             EVERY read is scoped by (tenant_id, user_id)
--                                             (src/notifications/handlers.rs:75-94,152) and all three
--                                             writers bind a user_id (src/worker.rs:147,344,
--                                             src/events/dispatcher.rs:197, src/automation/actions.rs:240).
--                                             A NULL user_id notification is invisible to every endpoint
--                                             and to every UI — it is not a preserved row, it is an
--                                             unreachable one.
--
-- The parent delete on the four `users` edges has no live caller (no user-delete surface in src/), so
-- nothing flips today; the arm is still decided from the read paths, because the first user-delete
-- feature or a hand-issued `DELETE FROM users` would otherwise 23503 immediately.
--
-- Residual, measured, NOT changed here: `events.company_id -> companies`, `events.deal_id ->
-- opportunities` and `link_clicks`' own `tenant_id` edge are unchanged by this file (the first two are
-- non-cascade edges off two other parents and are carded separately; the third is already CASCADE from
-- 091). This file arms exactly the 11 edges measured above and nothing else.
--
-- The full-chain path this file intersects (card item 3): 091 made `tenants` cascade to
-- `tracked_links`, `link_clicks`, `message_templates` and `notification_rules`, so ONE
-- `DELETE FROM tenants` now reaches `tracked_links -> link_clicks` and
-- `message_templates -> notification_rules` inside a single statement. A SET NULL arm on
-- `link_clicks` is impossible (NOT NULL) and any RESTRICT on that path would break retirement:
-- proven again after this migration in 02-dryrun.txt (the forward rehearsal deletes a workspace
-- holding a row in all 11 edges plus their nested children, and every row is gone).
--
-- Everything below was measured on LIVE inside BEGIN … ROLLBACK before this file was written: each of
-- the 11 edges raises 23503 before, every one of them lets the parent delete through after, the
-- whole-DB row-count fingerprint is identical around the migration, and the negative control (a
-- child row naming a missing parent is still refused with 23503) holds (01-blocked-before.txt,
-- 02-dryrun.txt, 06-live-api-pre.txt, 07-live-api-post.txt).

-- 1. NOT NULL columns: CASCADE is the only arm that lets the existing DELETE endpoint work.
ALTER TABLE link_clicks DROP CONSTRAINT IF EXISTS link_clicks_contact_id_fkey;
ALTER TABLE link_clicks ADD CONSTRAINT link_clicks_contact_id_fkey
    FOREIGN KEY (contact_id) REFERENCES contacts(id) ON DELETE CASCADE;

ALTER TABLE tracked_links DROP CONSTRAINT IF EXISTS tracked_links_tag_id_fkey;
ALTER TABLE tracked_links ADD CONSTRAINT tracked_links_tag_id_fkey
    FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE CASCADE;

ALTER TABLE link_clicks DROP CONSTRAINT IF EXISTS link_clicks_tracked_link_id_fkey;
ALTER TABLE link_clicks ADD CONSTRAINT link_clicks_tracked_link_id_fkey
    FOREIGN KEY (tracked_link_id) REFERENCES tracked_links(id) ON DELETE CASCADE;

ALTER TABLE notifications DROP CONSTRAINT IF EXISTS notifications_user_id_fkey;
ALTER TABLE notifications ADD CONSTRAINT notifications_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- 2. Nullable pointers whose worker keeps working without them: SET NULL.
ALTER TABLE notification_rules DROP CONSTRAINT IF EXISTS notification_rules_template_id_fkey;
ALTER TABLE notification_rules ADD CONSTRAINT notification_rules_template_id_fkey
    FOREIGN KEY (template_id) REFERENCES message_templates(id) ON DELETE SET NULL;

ALTER TABLE cs_messages DROP CONSTRAINT IF EXISTS cs_messages_parent_message_id_fkey;
ALTER TABLE cs_messages ADD CONSTRAINT cs_messages_parent_message_id_fkey
    FOREIGN KEY (parent_message_id) REFERENCES cs_messages(id) ON DELETE SET NULL;

ALTER TABLE cs_messages DROP CONSTRAINT IF EXISTS cs_messages_recipient_id_fkey;
ALTER TABLE cs_messages ADD CONSTRAINT cs_messages_recipient_id_fkey
    FOREIGN KEY (recipient_id) REFERENCES contacts(id) ON DELETE SET NULL;

ALTER TABLE events DROP CONSTRAINT IF EXISTS events_contact_id_fkey;
ALTER TABLE events ADD CONSTRAINT events_contact_id_fkey
    FOREIGN KEY (contact_id) REFERENCES contacts(id) ON DELETE SET NULL;

ALTER TABLE cs_messages DROP CONSTRAINT IF EXISTS cs_messages_sender_id_fkey;
ALTER TABLE cs_messages ADD CONSTRAINT cs_messages_sender_id_fkey
    FOREIGN KEY (sender_id) REFERENCES users(id) ON DELETE SET NULL;

ALTER TABLE email_campaigns DROP CONSTRAINT IF EXISTS email_campaigns_created_by_fkey;
ALTER TABLE email_campaigns ADD CONSTRAINT email_campaigns_created_by_fkey
    FOREIGN KEY (created_by) REFERENCES users(id) ON DELETE SET NULL;

ALTER TABLE events DROP CONSTRAINT IF EXISTS events_created_by_fkey;
ALTER TABLE events ADD CONSTRAINT events_created_by_fkey
    FOREIGN KEY (created_by) REFERENCES users(id) ON DELETE SET NULL;
