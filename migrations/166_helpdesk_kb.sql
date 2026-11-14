-- The knowledge base: what the helpdesk knows, so the same question is not answered twice.
--
-- This is the foundation the AI answers stand on, and it earns its place without any AI at all: a
-- published article can be found by a customer in the widget, and an agent can paste it into a reply.
-- Search is Postgres full-text from day one, with no extension to install — vector search was in the
-- spec, and it is NOT installed here (confirmed: no `vector` extension, Alpine/musl image), so the
-- `embedding` column is left out entirely rather than added empty and forgotten.
--
-- Two visibility rules are load-bearing, and both are enforced by the queries rather than by the UI:
--   * `status` — a draft is never visible to a customer.
--   * `visibility` — an INTERNAL article is knowledge for the team; it is never returned to a public
--     caller, ever, at any status. An article written to help agents decide a refund is not a page.

CREATE TABLE IF NOT EXISTS helpdesk_kb_articles (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    title       text NOT NULL,
    slug        text NOT NULL,
    body_md     text NOT NULL DEFAULT '',
    status      text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published', 'archived')),
    visibility  text NOT NULL DEFAULT 'public' CHECK (visibility IN ('public', 'internal')),
    locale      text NOT NULL DEFAULT 'en',
    author_id   uuid REFERENCES users(id) ON DELETE SET NULL,
    view_count  integer NOT NULL DEFAULT 0,
    helpful_yes integer NOT NULL DEFAULT 0,
    helpful_no  integer NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, slug)
);

-- The search index. A generated column keeps the vector in step with the text by construction: there is
-- no writer to forget, and no window where the body changed and the index did not.
ALTER TABLE helpdesk_kb_articles
    ADD COLUMN IF NOT EXISTS search tsvector
    GENERATED ALWAYS AS (
        to_tsvector('english', coalesce(title, '') || ' ' || coalesce(body_md, ''))
    ) STORED;

CREATE INDEX IF NOT EXISTS idx_helpdesk_kb_search
    ON helpdesk_kb_articles USING GIN (search);
CREATE INDEX IF NOT EXISTS idx_helpdesk_kb_public
    ON helpdesk_kb_articles (tenant_id, status, visibility);

-- Which articles a customer was shown, so deflection can be measured rather than guessed at.
CREATE TABLE IF NOT EXISTS helpdesk_kb_views (
    id         bigserial PRIMARY KEY,
    tenant_id  uuid NOT NULL,
    article_id uuid NOT NULL REFERENCES helpdesk_kb_articles(id) ON DELETE CASCADE,
    surface    text NOT NULL DEFAULT 'widget' CHECK (surface IN ('widget', 'portal', 'agent')),
    seen_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_helpdesk_kb_views_recent
    ON helpdesk_kb_views (tenant_id, seen_at DESC);
