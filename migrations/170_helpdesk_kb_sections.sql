-- A help centre a customer can actually browse: sections, and articles that belong to one.
--
-- The knowledge base already held articles and full-text search. What it could not do is answer the question a
-- stranger arrives with — "what do you have about billing?" — because a flat list of forty titles is not a
-- help centre. A section is the unit a person scans, and it is also what makes a search result explainable:
-- "Billing · How to change the card on file" says where it lives before you open it.
--
-- Sections are per account, ordered by hand (`position`), and an article may belong to none: an uncategorised
-- article still publishes and still searches. Refusing to publish it would punish somebody for tidiness they
-- had not got to yet.

CREATE TABLE IF NOT EXISTS helpdesk_kb_categories (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        text NOT NULL,
    slug        text NOT NULL,
    description text,
    position    integer NOT NULL DEFAULT 100,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_helpdesk_kb_categories_order
    ON helpdesk_kb_categories (tenant_id, position, name);

ALTER TABLE helpdesk_kb_articles
    ADD COLUMN IF NOT EXISTS category_id uuid
    REFERENCES helpdesk_kb_categories(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_helpdesk_kb_articles_category
    ON helpdesk_kb_articles (tenant_id, category_id)
    WHERE category_id IS NOT NULL;

-- "Was this helpful?" needs to be answerable per visitor without a login, and without one person counting
-- forty times. The vote rows are the evidence behind the counters on the article.
CREATE TABLE IF NOT EXISTS helpdesk_kb_votes (
    id         bigserial PRIMARY KEY,
    tenant_id  uuid NOT NULL,
    article_id uuid NOT NULL REFERENCES helpdesk_kb_articles(id) ON DELETE CASCADE,
    helpful    boolean NOT NULL,
    seen_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_helpdesk_kb_votes_article
    ON helpdesk_kb_votes (article_id, seen_at DESC);
