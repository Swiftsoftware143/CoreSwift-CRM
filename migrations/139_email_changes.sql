-- 139_email_changes.sql — the sign-in address change (kanban t_bd31809e).
--
-- Fleet parity: this is the SAME table ADASwift ships as `014_email_changes.sql`, and the same shape
-- FunnelSwift / IncentiveSwift / WorkflowSwift / missedcallrespondr use. CoreSwift was the only app
-- whose customer could not move their own sign-in address — it had no route, no page and no control,
-- so an address change meant asking support to move the account by hand.
--
-- ONE pending request per account: the request arm retires the caller's outstanding rows, and the
-- confirm arm claims a row in a single statement (`used = false` -> `true` with RETURNING), so two
-- concurrent opens cannot both win and a reload can never redeem a link twice. `token` is the
-- credential the customer receives by mail; `users.email` does not move until it is redeemed.
--
-- Idempotent by construction: a fresh database and the live one both end in the same state.

CREATE TABLE IF NOT EXISTS email_changes (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    new_email   TEXT NOT NULL,
    token       TEXT NOT NULL UNIQUE,
    expires_at  TIMESTAMPTZ NOT NULL,
    used        BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- The request arm retires the caller's outstanding rows, and the Profile screen asks whether one is
-- already live; both want this rather than a full-table scan.
CREATE INDEX IF NOT EXISTS email_changes_user_live_idx
    ON email_changes (user_id) WHERE used = FALSE;
