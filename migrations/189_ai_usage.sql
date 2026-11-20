-- AI usage, per account (ticketing suite item 5: per-tenant AI keys).
--
-- WHY A TABLE AND NOT A LOG LINE: an account that brings its own model key is paying the provider directly,
-- and an account running on the platform's key is spending OUR money. Neither fact was recorded anywhere,
-- so "which workspace used the AI this month, and with whose key" had no answer at all. This is that answer.
--
-- The row is written for FAILED calls too: a provider that refuses our key is exactly the thing an operator
-- needs to see, and a usage table that only counts successes cannot show it.

CREATE TABLE IF NOT EXISTS ai_usage (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    feature      text NOT NULL,                    -- what asked: 'ticket_reply', 'campaign', 'learn', ...
    provider     text NOT NULL,                    -- 'deepseek' | 'openai' | 'anthropic'
    model        text,                             -- the model name the provider was asked for
    tokens_in    integer NOT NULL DEFAULT 0,
    tokens_out   integer NOT NULL DEFAULT 0,
    platform_key boolean NOT NULL DEFAULT false,   -- true = OUR credential paid for this call
    ok           boolean NOT NULL DEFAULT true,
    detail       text,                             -- the error, when ok = false
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_ai_usage_tenant_time ON ai_usage (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_ai_usage_platform ON ai_usage (created_at DESC) WHERE platform_key;
