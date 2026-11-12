-- 146 — the hosted calendar address of every workspace (kanban t_8457bfef).
--
-- EVERY account gets one working calendar address out of the box: a unique subdomain under the
-- system domain (coreswiftcrm.com). An account that owns a domain can point it at the same
-- calendar instead, and the system subdomain keeps answering as the fallback.
--
-- Uniqueness is enforced by the DATABASE, not by the handler that happens to write a row:
--   * one row per tenant            UNIQUE (tenant_id)
--   * one owner per subdomain       UNIQUE (lower(subdomain))
--   * one owner per custom domain   UNIQUE (lower(custom_domain)) WHERE custom_domain IS NOT NULL
-- The reserved-name CHECK keeps a workspace from ever claiming app / admin / inbox / mail — the
-- system's own hostnames. The format CHECKs keep a value that cannot be a DNS label out of the
-- table, so a bad write is refused at the door instead of 404ing on the edge forever.

CREATE TABLE IF NOT EXISTS calendar_hosts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL UNIQUE REFERENCES tenants(id) ON DELETE CASCADE,
    subdomain TEXT NOT NULL,
    custom_domain TEXT,
    custom_domain_status TEXT NOT NULL DEFAULT 'none',   -- none | pending | verified
    custom_domain_checked_at TIMESTAMPTZ,
    custom_domain_last_error TEXT,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT calendar_hosts_subdomain_format
        CHECK (subdomain ~ '^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$'),
    CONSTRAINT calendar_hosts_subdomain_not_system
        CHECK (subdomain NOT IN (
            'app','www','admin','api','apex','inbox','pmail','mail','email','smtp','imap','pop',
            'pop3','webmail','mx','mxa','mxb','ns1','ns2','ns','cdn','static','assets','media',
            'status','docs','help','support','billing','payments','checkout','calendar',
            'calendars','book','booking','bookings','crm','dashboard','login','logout','register',
            'signup','sign-up','account','accounts','auth','portal','blog','news','dev','staging',
            'test','staging1','origin','secure','vpn','ftp','whois','verify','downloads','files'
        )),
    CONSTRAINT calendar_hosts_custom_domain_format
        CHECK (custom_domain IS NULL OR custom_domain ~ '^([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}$'),
    CONSTRAINT calendar_hosts_custom_domain_lower
        CHECK (custom_domain IS NULL OR custom_domain = lower(custom_domain)),
    CONSTRAINT calendar_hosts_custom_domain_not_system
        CHECK (custom_domain IS NULL OR (lower(custom_domain) <> 'coreswiftcrm.com'
           AND lower(custom_domain) NOT LIKE '%.coreswiftcrm.com')),
    CONSTRAINT calendar_hosts_custom_domain_status_valid
        CHECK (custom_domain_status IN ('none','pending','verified'))
);

-- One owner per subdomain, case-insensitively. The table's own CHECK already forces lowercase,
-- but the index is the thing the handler's INSERT can never slip past.
CREATE UNIQUE INDEX IF NOT EXISTS calendar_hosts_subdomain_uniq
    ON calendar_hosts (lower(subdomain));

-- One owner per custom domain. PARTIAL: every account without a domain holds NULL.
CREATE UNIQUE INDEX IF NOT EXISTS calendar_hosts_custom_domain_uniq
    ON calendar_hosts (lower(custom_domain))
    WHERE custom_domain IS NOT NULL;

-- The public resolver looks a request's Host up in BOTH columns, so it needs both sides indexed.
CREATE INDEX IF NOT EXISTS calendar_hosts_subdomain_lookup ON calendar_hosts (lower(subdomain));
CREATE INDEX IF NOT EXISTS calendar_hosts_custom_domain_lookup ON calendar_hosts (lower(custom_domain));

-- Backfill: every workspace that exists today gets its address now. The base is the workspace
-- slug reduced to DNS-label characters. Duplicates (two slugs that reduce to the same label) get
-- an ordinal suffix, so the unique index can never refuse this file and abort a boot.
WITH base AS (
    SELECT t.id,
           t.created_at,
           COALESCE(
               NULLIF(btrim(lower(regexp_replace(t.slug, '[^a-zA-Z0-9]+', '-', 'g')), '-'), ''),
               'workspace'
           ) AS b
    FROM tenants t
), trimmed AS (
    SELECT id, created_at, left(b, 55) AS b FROM base
), ranked AS (
    SELECT id, created_at, b,
           row_number() OVER (PARTITION BY b ORDER BY created_at, id) AS rn
    FROM trimmed
)
INSERT INTO calendar_hosts (tenant_id, subdomain)
SELECT r.id,
       CASE
           WHEN r.rn = 1 AND r.b NOT IN (
               'app','www','admin','api','apex','inbox','pmail','mail','email','smtp','imap','pop',
               'pop3','webmail','mx','mxa','mxb','ns1','ns2','ns','cdn','static','assets','media',
               'status','docs','help','support','billing','payments','checkout','calendar',
               'calendars','book','booking','bookings','crm','dashboard','login','logout','register',
               'signup','sign-up','account','accounts','auth','portal','blog','news','dev','staging',
               'test','staging1','origin','secure','vpn','ftp','whois','verify','downloads','files'
           )
           THEN r.b
           ELSE r.b || '-' || r.rn
       END
FROM ranked r
ON CONFLICT DO NOTHING;
