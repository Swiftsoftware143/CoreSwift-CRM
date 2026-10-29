-- The latch behind the "choose your own password" door (kanban t_d841df4f).
--
-- Signup mints the first password and MAILS it; the mail is the customer's only way in. When that
-- mail cannot be delivered (the provider refuses, or the address is one the sender withholds) the
-- account exists with a password nobody has ever seen, and until now the signup page still said
-- "Check your email — we sent your login details" — a lie — leaving the customer with an account
-- they could not enter and no self-service way out (the reset page travels the SAME mail path that
-- had just failed).
--
-- `password_undelivered` is that exact state and nothing else:
--   * SET by `auth::handlers::register` when it MINTED the password and the send did not go out;
--   * CLEARED by `POST /api/auth/initial-password`, which writes the password the customer chooses
--     in the SAME statement that clears it, so the door is one-shot;
--   * CLEARED by the reset and change-password writers, which set a password the customer knows.
--
-- Same shape as the sibling apps (WorkflowSwift, missedcallrespondr, ADASwift, IncentiveSwift).
-- Idempotent: this app runs `sqlx::migrate!("./migrations")`, so the file is applied once and keyed
-- by its version prefix; never edit it after it has been applied.
ALTER TABLE users ADD COLUMN IF NOT EXISTS password_undelivered boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN users.password_undelivered IS
    'true only when signup minted a first password and its credentials mail did not reach the customer; cleared by the first initial-password set, a password reset, or a password change';
