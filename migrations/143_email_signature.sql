-- 143_email_signature.sql
--
-- THE COMPANY SIGNATURE (David, 2026-11-11): one logo per workspace, a plain signature of
-- name / title / company / phone / one line, a WORKSPACE DEFAULT plus a per-mailbox override, and
-- both already in place when Compose opens.
--
-- WHY A TABLE AND COLUMNS INSTEAD OF JSONB. `tenants.settings -> 'email_branding'` is edited by a
-- console screen that does a full-document read-modify-write; parking the signature in the same
-- document would mean that save can silently drop it (or the signature editor can drop the brand
-- name). One writer per field: branding keeps its document, this migration gives the signature its
-- own store.
--
-- WHY width_px / height_px ON tenant_logos. The logo is resized ONCE at upload to a mail-safe size
-- (and 2x for sharpness), and the HTML carries width/height attributes because Outlook collapses an
-- image it cannot measure before loading. Without the stored pixel size the renderer would have to
-- guess, and a wrong guess is a stretched or oversized logo in a customer's inbox.
--
-- The legacy free-text `private_email_boxes.signature` column is NOT dropped and NOT backfilled: a
-- mailbox that has it keeps it (the renderer appends those lines after the structured fields), so
-- nothing a customer typed disappears because a field appeared next to it.

ALTER TABLE tenant_logos ADD COLUMN IF NOT EXISTS width_px integer;
ALTER TABLE tenant_logos ADD COLUMN IF NOT EXISTS height_px integer;

CREATE TABLE IF NOT EXISTS tenant_email_signatures (
    tenant_id uuid PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    full_name  varchar(120) NOT NULL DEFAULT '',
    job_title  varchar(120) NOT NULL DEFAULT '',
    company    varchar(120) NOT NULL DEFAULT '',
    phone      varchar(60)  NOT NULL DEFAULT '',
    line       varchar(160) NOT NULL DEFAULT '',
    updated_at timestamptz  NOT NULL DEFAULT now()
);

-- Per-mailbox override. `signature_mode` is the whole choice, in three states a person can
-- understand on screen:
--   inherit -> use the workspace default (a mailbox that has never been configured)
--   custom  -> use this mailbox's own fields; a blank field falls back to the workspace default
--   off     -> never add a signature from this address (a no-reply or support queue)
ALTER TABLE private_email_boxes ADD COLUMN IF NOT EXISTS signature_mode varchar(16) NOT NULL DEFAULT 'inherit';
ALTER TABLE private_email_boxes ADD COLUMN IF NOT EXISTS signature_full_name varchar(120);
ALTER TABLE private_email_boxes ADD COLUMN IF NOT EXISTS signature_job_title varchar(120);
ALTER TABLE private_email_boxes ADD COLUMN IF NOT EXISTS signature_company varchar(120);
ALTER TABLE private_email_boxes ADD COLUMN IF NOT EXISTS signature_phone varchar(60);
ALTER TABLE private_email_boxes ADD COLUMN IF NOT EXISTS signature_line varchar(160);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'private_email_boxes_signature_mode_check'
    ) THEN
        ALTER TABLE private_email_boxes
            ADD CONSTRAINT private_email_boxes_signature_mode_check
            CHECK (signature_mode IN ('inherit', 'custom', 'off'));
    END IF;
END
$$;
