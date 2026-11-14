-- Response clocks.
--
-- A support queue without a clock is a list of things somebody meant to answer. This migration gives
-- every account one policy row and stamps each new ticket with the two moments it must be answered and
-- resolved by, which is what lets the console say "due in 40 minutes" and "breached 3 hours ago".
--
-- Deliberately excluded: business-hours arithmetic and pause-while-pending. Both are real (the spec asks
-- for them) and both change the shape of the calculation rather than the storage, so they arrive with the
-- worker that also sends the breach events. Until then a clock that is honestly wall-clock is better than
-- one that pretends to know the calendar and does not.
--
-- The stamping is a trigger for the same reason the ticket number is: five paths insert a ticket today and
-- more are coming, and a rule that lives in one of them is a rule the others have already broken.

CREATE TABLE IF NOT EXISTS helpdesk_sla_policies (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id          uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name               text NOT NULL,
    -- minutes, per priority: {"urgent": {"first": 15, "resolve": 240}, ...}
    targets            jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- used when a priority has no explicit target
    default_first_response_mins integer NOT NULL DEFAULT 480,
    default_resolution_mins     integer NOT NULL DEFAULT 2880,
    is_default         boolean NOT NULL DEFAULT false,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

-- The columns the clocks are written into. They were named in the mapping note but never created:
-- migration 162 carried `first_responded_at` and `solved_at` (what happened), and this carries the
-- deadlines (what was promised) plus the policy they came from — the pair a breach is judged between.
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS first_response_due timestamptz;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS resolution_due timestamptz;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS sla_policy_id uuid
    REFERENCES helpdesk_sla_policies(id) ON DELETE SET NULL;

-- One default policy per account, so the trigger always has something to read.
CREATE UNIQUE INDEX IF NOT EXISTS idx_helpdesk_sla_one_default
    ON helpdesk_sla_policies (tenant_id) WHERE is_default;

INSERT INTO helpdesk_sla_policies (tenant_id, name, is_default)
SELECT t.id, 'Standard', true
  FROM tenants t
 WHERE NOT EXISTS (SELECT 1 FROM helpdesk_sla_policies p WHERE p.tenant_id = t.id);

CREATE OR REPLACE FUNCTION helpdesk_stamp_sla() RETURNS trigger AS $$
DECLARE
    policy    helpdesk_sla_policies%ROWTYPE;
    first_min integer;
    res_min   integer;
BEGIN
    IF NEW.first_response_due IS NOT NULL OR NEW.resolution_due IS NOT NULL THEN
        RETURN NEW; -- a caller supplied its own clocks; leave them alone
    END IF;

    SELECT * INTO policy
      FROM helpdesk_sla_policies
     WHERE tenant_id = COALESCE(NEW.tenant_id, '00000000-0000-0000-0000-000000000000'::uuid)
       AND is_default
     LIMIT 1;

    IF NOT FOUND THEN
        RETURN NEW; -- no policy, no clock: better an empty column than a wrong deadline
    END IF;

    first_min := COALESCE((policy.targets -> NEW.priority ->> 'first')::int,
                          policy.default_first_response_mins);
    res_min   := COALESCE((policy.targets -> NEW.priority ->> 'resolve')::int,
                          policy.default_resolution_mins);

    NEW.sla_policy_id    := policy.id;
    NEW.first_response_due := NEW.created_at + make_interval(mins => first_min);
    NEW.resolution_due     := NEW.created_at + make_interval(mins => res_min);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_helpdesk_stamp_sla ON tickets;
CREATE TRIGGER trg_helpdesk_stamp_sla
    BEFORE INSERT ON tickets
    FOR EACH ROW EXECUTE FUNCTION helpdesk_stamp_sla();

-- "Due soon" is the view an agent works from, so it gets its own index.
CREATE INDEX IF NOT EXISTS idx_tickets_first_response_due
    ON tickets (tenant_id, first_response_due)
    WHERE first_responded_at IS NULL AND status IN ('new', 'open', 'pending', 'on_hold');

CREATE INDEX IF NOT EXISTS idx_tickets_resolution_due
    ON tickets (tenant_id, resolution_due)
    WHERE solved_at IS NULL AND status IN ('new', 'open', 'pending', 'on_hold');
