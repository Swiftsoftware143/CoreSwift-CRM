-- Send a visitor somewhere when they open one of YOUR addresses.
--
-- Why (David, 2026-11-25): "can you set it up so the user can put a redirect for the domain or subdomain?
-- That will be closing up a loop… and that feature should be found anywhere that users put their custom
-- domain, even sender domain."
--
-- An owner points several names at this platform: their account booking address, a per-workspace booking
-- address, a calendar's own address, and the domain they SEND email from. Some of those have no page to
-- show — a sending domain opens to nothing at all, which looks broken to anyone who types it. One table
-- keyed by HOSTNAME covers every surface, so a redirect is configured in one place and does not need a new
-- column on each table (which is how the second and third surface end up forgotten).
--
-- The host is unique platform-wide on purpose: two accounts cannot both own the same name, so the loser
-- gets a refusal that names the other account rather than a silent overwrite of someone's live redirect.

CREATE TABLE IF NOT EXISTS domain_redirects (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- lowercased hostname, no scheme, no path
    host text NOT NULL,
    -- where a visitor is sent: an absolute https/http URL, or a path on this site
    target_url text NOT NULL,
    -- 301 (permanent) or 302 (temporary). 302 by default: a redirect a person set today is not
    -- automatically a promise about the future.
    status_code integer NOT NULL DEFAULT 302,
    is_active boolean NOT NULL DEFAULT true,
    note text,
    created_at timestamp with time zone NOT NULL DEFAULT now(),
    updated_at timestamp with time zone NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS domain_redirects_host_key
    ON domain_redirects (lower(host));

CREATE INDEX IF NOT EXISTS domain_redirects_tenant_idx
    ON domain_redirects (tenant_id);

DO $$
BEGIN
    ALTER TABLE domain_redirects
        ADD CONSTRAINT domain_redirects_status_chk
        CHECK (status_code IN (301, 302, 307, 308));
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

-- A target is only ever an http(s) address or a path. This is not a formatting preference: the value
-- becomes a browser navigation for whoever opens the owner's domain, so `javascript:` or `data:` in this
-- column would be a stored attack, and `//host` would silently send visitors to a THIRD party.
DO $$
BEGIN
    ALTER TABLE domain_redirects
        ADD CONSTRAINT domain_redirects_target_chk
        CHECK (target_url ~ '^(https?://[^[:space:]]+|/[^/[:space:]][^[:space:]]*)$');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

COMMENT ON TABLE domain_redirects IS
    'Where a visitor is sent when they open an address this account owns (booking address, workspace '
    'address, calendar address, sending domain). One row per hostname.';

COMMENT ON COLUMN domain_redirects.host IS
    'Lowercased hostname with no scheme or path. Unique platform-wide so two accounts cannot fight over '
    'one name.';
