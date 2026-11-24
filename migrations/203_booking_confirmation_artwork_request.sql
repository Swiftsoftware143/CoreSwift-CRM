-- The confirmation email asks for artwork.
--
-- Why (ZaarHub, 2026-11-24): a sponsor has to send artwork when they book, and the only moment they are
-- paying attention is the confirmation they already receive. The message was real but invisible — it
-- comes from this platform default, so the account had no template to point at and no way to see or
-- change the wording — and it asked for nothing.
--
-- The block below states the position, the artwork size and the date, all from the booking itself
-- ({{artwork_block}}), so the spec cannot drift from what is sold. The "already sent us artwork" line is
-- deliberate: without it every repeat sponsor replies "I already sent that", which buries the sponsors who
-- genuinely have to act.
--
-- Guarded on the OLD body on purpose: a workspace that has already edited its own wording is never
-- overwritten, and this only fills in the platform default it shipped with.

UPDATE email_templates
   SET body = 'Hello {{customer_name}},' || chr(10) || chr(10) ||
              'Your booking with {{calendar_name}} is confirmed.' || chr(10) || chr(10) ||
              '{{items}}' || chr(10) || chr(10) ||
              'Total: {{total}}' || chr(10) || chr(10) ||
              'YOUR AD - ACTION NEEDED' || chr(10) || chr(10) ||
              'To get your ad into the newsletter, reply to this email with your artwork attached.' || chr(10) || chr(10) ||
              '{{artwork_block}}' || chr(10) ||
              '  Format:    JPG or PNG' || chr(10) || chr(10) ||
              'Already sent us artwork on a previous booking and don''t want to change it? Just ignore this - your existing creative stays in place.' || chr(10) || chr(10) ||
              'Reply here with any questions.' || chr(10) || chr(10) ||
              'Need to change the booking? Open this link - you can move it, cancel it, or book it again:' || chr(10) ||
              '{{manage_url}}' || chr(10) || chr(10) ||
              'Just reply to this email if you would rather speak to a person.' || chr(10),
       updated_at = now()
 WHERE template_type = 'booking_confirmed'
   AND is_default = true
   AND body LIKE 'Hello {{customer_name}},%'
   AND body NOT LIKE '%artwork_block%';

COMMENT ON COLUMN email_templates.body IS
    'The message body with merge fields. booking_confirmed carries {{artwork_block}} so the artwork request states each position, its size and its date from the booking itself.';
