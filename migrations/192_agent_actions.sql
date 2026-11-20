-- The action agent's ledger, and the money rule that governs it.
--
-- ITEM 1 OF THE TICKETING SUITE, built last because it is the only part that CHANGES A CUSTOMER'S RECORD.
-- Everything else in the suite observes: it reads mail, tags a tone, mails a person. This one acts. So the
-- order here is reversed from a normal feature: the ledger and the limits exist BEFORE any action does.
--
-- Decision on record (docs/ticketing-suite-plan.md, decision 2): a limit PER ACTION, capped by a
-- TENANT-WIDE CEILING. An action's limit can never exceed the ceiling, and anything above either number
-- goes to a person. That is enforced in the table (a CHECK), not only in the handler: a limit that could
-- exceed its own ceiling is a configuration that should be impossible to store, not merely discouraged.

CREATE TABLE IF NOT EXISTS agent_limits (
    tenant_id        uuid PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    -- The most any ONE action may cost. Zero means "no action of mine spends money at all".
    per_action_cents bigint NOT NULL DEFAULT 0 CHECK (per_action_cents >= 0),
    -- The most this workspace will ever let the agent spend in a single action, whatever the per-action
    -- figure says. Two numbers so that raising one by mistake cannot raise the other.
    ceiling_cents    bigint NOT NULL DEFAULT 0 CHECK (ceiling_cents >= 0),
    -- Agents are OFF until a workspace turns them on. A feature that can act on customers should not
    -- arrive switched on for accounts that never asked for it.
    enabled          boolean NOT NULL DEFAULT false,
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT agent_limit_within_ceiling CHECK (per_action_cents <= ceiling_cents)
);

-- Every action the agent proposes, whoever decided it, and what actually happened.
--
-- `status` is the whole safety story in one column:
--   proposed  the agent wants to do this and nobody has said yes
--   approved  a person said yes; not yet carried out
--   refused   a person said no (kept, because "the agent asked to do X and was told no" is a record)
--   executed  carried out, with the result
--   failed    carrying out failed, with the reason — never silently dropped
CREATE TABLE IF NOT EXISTS agent_actions (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    ticket_id      uuid REFERENCES tickets(id) ON DELETE CASCADE,
    kind           text NOT NULL,
    payload        jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- What this action costs, so the limit can be applied to the SAME number that is stored, instead of a
    -- number computed twice.
    cost_cents     bigint NOT NULL DEFAULT 0 CHECK (cost_cents >= 0),
    status         text NOT NULL DEFAULT 'proposed'
                   CHECK (status IN ('proposed', 'approved', 'refused', 'executed', 'failed')),
    -- Why the agent wanted it, in its own words, and — when it was held — the fact that it was held.
    reason         text,
    held_because   text,
    proposed_by    text NOT NULL DEFAULT 'agent',
    decided_by     text,
    decided_at     timestamptz,
    executed_at    timestamptz,
    result         jsonb,
    detail         text,
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_agent_actions_tenant_time ON agent_actions (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_actions_waiting
    ON agent_actions (tenant_id, created_at DESC) WHERE status IN ('proposed', 'approved');
CREATE INDEX IF NOT EXISTS idx_agent_actions_ticket ON agent_actions (ticket_id, created_at DESC);

-- The catalogue of things the agent may ask for. A kind not in this table cannot be proposed at all, which
-- is what stops a bug from inventing an action nobody reviewed.
CREATE TABLE IF NOT EXISTS agent_action_kinds (
    kind        text PRIMARY KEY,
    label       text NOT NULL,
    -- true = this kind can move money, so the limits apply to it.
    spends      boolean NOT NULL DEFAULT false,
    description text NOT NULL
);

INSERT INTO agent_action_kinds (kind, label, spends, description) VALUES
    ('ticket_note',     'Leave an internal note',  false, 'Writes a note on the ticket that the customer never sees.'),
    ('set_priority',    'Change the ticket priority', false, 'Moves the ticket to low, medium, high or urgent, which re-clocks its deadlines.'),
    ('tag_contact',     'Tag the customer',        false, 'Applies one tag to the person who wrote in.'),
    ('add_to_list',     'Add the customer to a list', false, 'Puts the person who wrote in on one of your lists, which can start an automation.'),
    ('assign_ticket',   'Hand the ticket to a teammate', false, 'Assigns the ticket to one member of the account.'),
    ('escalate',        'Escalate the ticket',     false, 'Marks the ticket urgent and tells the account owner through the escalation ladder.')
ON CONFLICT (kind) DO UPDATE SET
    label = EXCLUDED.label,
    spends = EXCLUDED.spends,
    description = EXCLUDED.description;
