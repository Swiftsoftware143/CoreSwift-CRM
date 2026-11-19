# Automation sending lane — the decision, and how it will be built

Written 2026-11-18 so the next session starts from the reasoning rather than re-deriving it.

## What industry practice says, and why it matches the recorded decision

Bulk/automated mail is separated from human mail by **sending identity**, not by screen:

- **Reputation is per-domain (and per-IP-pool).** Complaint and bounce rates are measured against the domain
  mail is sent from. A blast that draws complaints takes the whole domain's deliverability with it — including
  the replies a customer is waiting for. A subdomain (`mg.<domain>`) carries its own reputation, so a bad
  campaign cannot silence a real conversation.
- **Subdomain of the brand domain, not a different domain.** `mg.brand.com` keeps brand trust and lets SPF/DKIM/
  DMARC be tightened per lane without touching the root's records.
- **Replies must land where a human reads them.** The automated lane sets `Reply-To` to a real mailbox on the main
  domain, so a customer who answers a campaign reaches a person.
- **Never bulk-send from the root.** This is the rule the fleet already half-follows: platform mail uses
  `mail.coreswiftcrm.com`, and per-brand lanes exist (`mg.swiftsoftware.net`, `mg.swiftimpactsolutions.com`),
  but no per-ACCOUNT automation identity is wired, so automation currently sends from the main domain and mixes
  the two lanes. That is the gap (`B207`).

So the decision recorded on 2026-11-10 is correct: **private mailbox on the main domain, automation on a
subdomain of it.** The user interface does not move — no new window, no separate host. Only the From identity
changes.

## How it will be built (no migration)

`tenants.settings` is a jsonb column that already exists, so the per-account automation identity can be stored
without a schema change:

```json
{ "automation": { "from_domain": "mg.example.com", "from_name": "Example Updates", "reply_to": "support@example.com" } }
```

1. **Read path**: one helper `automation_identity(tenant)` returning `(from_domain, from_name, reply_to)` with
   defaults derived from the account's verified primary domain (`mg.<domain>`, name = account name,
   reply_to = the account's support mailbox).
2. **Write path**: the automation/campaign sender uses that helper instead of the main-domain address, and sets
   `Reply-To` to the mailbox. Private-email human sends are untouched.
3. **Guard**: refusing to send automation from the main domain when a subdomain is configured — a mixed lane is
   what this card exists to end.
4. **UI**: the Email Domains screen shows the automation sending address, with a hint that it must be verified in
   the provider (Mailgun) before use, and a one-click "use mg.<domain>" default.
5. **Proof**: a campaign send and a human reply in the same account, showing two different From domains, with the
   reply landing in the inbox, plus a tenancy test that another account's identity is never read.

## Order of work after this

`B207` → `B208` (provider-agnostic sending) → `B201`/`B203` (workspace management) → `B204` (team login and inbox
assignment) → `B209` (feedback + public roadmap/updates) → `B212` (MCP coverage, needs a real count) →
`B213` (calendar custom domain, still parked by David).

Anything requiring a credential or an account only David can create stays with him: Mailgun/DNS verification for
new subdomains, payment provider keys, and anything that spends money.

---

# One domain, three lanes (David, 2026-11-18)

A customer who brings a domain should not have to bring it three times. The account adds **one domain**, then
assigns the **lanes** it wants. Each lane is independently verified, because each needs its own DNS records and
each carries its own reputation.

| Lane | Address shape | What it is | Reputation cost of getting it wrong |
|---|---|---|---|
| **Inbox** | `support@their-domain.com`, `david@their-domain.com` | The main domain. Warm, human, hand-typed replies. | Highest — this is where customers answer. |
| **Automation** | `mg.their-domain.com` (or `mail.`) | Sequences, auto-responders, campaigns. `Reply-To` points at the inbox lane. | Must never touch the inbox lane's standing. |
| **Calendar** | `booking.their-domain.com` (or `calendar.`) | Public booking pages for their customers. | Low for mail, high for brand: a broken booking page is a lost appointment. |

## The rule that keeps this simple

**A lane is offered only when its own records verify.** An account that has verified `their-domain.com` for inbox
mail but not `mg.their-domain.com` sees automation as *available but not yet verified*, with the exact records to
add — never a control that silently sends from the wrong address.

Same pattern for calendar: the host name is created, but the page is not advertised at that address until the
record resolves. Until then it serves from the platform host (today: `inbox.coreswiftcrm.com` /
`booking.coreswiftcrm.com`), which is also the no-custom-domain default, so nothing is ever dead.

## Storage (still no migration)

All three live in `tenants.settings.domains`:

```json
{ "domains": {
    "inbox":      { "domain": "their-domain.com",        "verified": true,  "records": {...} },
    "automation": { "from_domain": "mg.their-domain.com", "verified": false },
    "calendar":   { "host": "booking.their-domain.com",   "verified": false }
} }
```

`private_email_domains` keeps its own rows for the inbox lane (it already has provider credentials and catch-all
per domain) and remains the source of truth for mail. `tenants.settings.domains` holds the lane assignment and the
verification state for the two lanes that have no table of their own.

## Build order, revised

This absorbs `B213` (calendar custom domain) — it is no longer parked, it is the third lane of the same screen.

1. The Domains screen: one domain, three lanes, each with its own status and the records to add.
2. Inbox lane: already works; it becomes lane one of this screen rather than a separate panel.
3. Automation lane: the send path from the previous section, plus per-lane `Reply-To`.
4. Calendar lane: the host table already exists; add the verified gate before advertising the address.
5. Proof, per lane: a real message and a real booking sent/served from the custom address, plus a tenancy test
   showing one account can never read another's domain settings.
