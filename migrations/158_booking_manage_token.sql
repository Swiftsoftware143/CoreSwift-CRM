-- 158_booking_manage_token.sql
--
-- A booking needs a way for the customer to manage it without an account: reschedule, cancel, rebook.
-- The link carries the authority, so the token must be unguessable and unique, and it is scoped to
-- ONE order.
--
-- Deliberately NOT a signed value derived from the id: an opaque random token means a leaked id
-- reveals nothing, and revoking one order cannot be forged from another. 64 hex characters of
-- gen_random_uuid() entropy, minted by the database so every writer gets one for free.

alter table booking_orders add column if not exists manage_token text;

update booking_orders
   set manage_token = replace(gen_random_uuid()::text, '-', '') || replace(gen_random_uuid()::text, '-', '')
 where manage_token is null;

alter table booking_orders alter column manage_token set not null;
alter table booking_orders
    alter column manage_token set default
    replace(gen_random_uuid()::text, '-', '') || replace(gen_random_uuid()::text, '-', '');

create unique index if not exists idx_booking_orders_manage_token on booking_orders (manage_token);

comment on column booking_orders.manage_token is
    'Opaque credential for the customer-facing manage link. Scoped to one order; never derived from the row id.';
