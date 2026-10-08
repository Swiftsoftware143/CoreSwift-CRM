# Private Email — explicit scope

**Status: declaration, not aspiration.** This file records what CoreSwift CRM's private email
*is*, and — just as explicitly — the three capabilities it is **not**, so that a future grep
census of `src/private_email/` reads "out of scope" instead of "silently missing".

Declared 2026-10-08 under kanban **t_04bdfc5e**. Supersedes the "private email parity finished"
reading of **t_6597a567**: parity with a mailbox *host* is **not** claimed and never was.

---

## What private email IS (shipped, in scope)

Module `src/private_email/` (12 `.rs` files + `providers/{mailgun,smtp}.rs`), mounted at
`/api/private-email/*`, plus the inbound webhook at `POST /api/private-email/webhook`
(`src/main.rs:602`). It is a **send + route** capability, not a mailbox host:

- **Custom domains** — add / update / delete / re-verify against the provider
  (`domain_handler.rs`).
- **Provider credentials** — Mailgun and SMTP today; named, reusable API keys.
  Credentials are encrypted at rest, per-tenant, AES-256-GCM, fail-closed (`encryption.rs`,
  `api_keys_handler.rs`).
- **Mailboxes** — provision / list / update / delete addresses on your domain
  (`mailbox_handler.rs`).
- **Sending** — outbound through the tenant's own provider, so their DKIM/SPF signs it
  (`send_handler.rs`, `providers/`).
- **Auto-reply / forwarding** — trigger-based replies per mailbox (`auto_reply_handler.rs`).
- **Inbound (webhook only)** — the provider POSTs to `/api/private-email/webhook`; the app
  matches the sender to a contact, records contact activity, opens a support ticket and fires
  auto-replies (`webhook_handler.rs`).
- **Admin** — per-tenant limits, retention settings, purge (`admin_handler.rs`, `purge.rs`),
  plan gating (`feature_gate.rs`).

## What private email is NOT — explicitly out of scope

| Capability | State | Why it is out of scope |
|---|---|---|
| **Webmail / hosted inbox** | Not built | There is **no message store**: no `private_email_messages` table (`grep -rn private_email_messages migrations/ src/` → 0). Outbound sends are not persisted (a send writes an `events` row); inbound mail survives only as contact activity / a ticket. A hosted inbox is a product surface in its own right, not a fix-up. |
| **AI email assistant** | Not built | The AI Assistant tab is the general workspace assistant; nothing in it is email-aware (`src/` has no email-assistant code). Making it email-aware needs the message store above to read from. |
| **Calendar & contacts sync (CalDAV / CardDAV)** | Not built | The CRM has app-native calendar (`booking_calendars`, `calendar_slots`) and contacts, but **no DAV endpoint** (`grep -ril 'caldav\|carddav' src/` → 0), so Apple Mail / Thunderbird / Outlook cannot sync them. |

Also declared, so they are not read as gaps:

- **Inbound retrieval is webhook-only.** No IMAP/POP3 polling
  (`providers/smtp.rs:3` says so in the code). A domain on the SMTP provider can send; inbound
  requires the provider to POST the webhook.
- **Anti-spam is delegated.** No inbound filter/blocklist in the app; filtering is the mail
  provider's, plus the app's own request rate limiter.

## Consequence for the served surfaces (fixed under t_04bdfc5e)

The marketing page and the tenant app used to describe a hosted **inbox** that does not exist.
That is a false claim about a shipped product, so the copy now matches the send/route reality
(fleet doctrine CS-23, CoreSwift claim-verification 2026-09-22: make the product match the
claim, or correct the claim):

- `public/index.html` — comparison row "Private Email Inbox" → "Private Email (Send & Route)";
  the "no third-party inbox … your email lives where your leads live" subtitle → describes
  sending from your own mailbox and replies landing on the contact timeline; "AES-256-GCM
  encryption on every message" → "on your provider credentials".
- `www-app/coreswift/index.html` — the Private Email tab no longer promises "Read and reply to
  a message"; it sends from the mailbox.

## Changing this

Building any of the three is a **product** decision, not a defect fix. It is tracked on kanban
**t_1f5077bf** (David's go/no-go). If a surface is built, delete its row from the table above
and move it into the "IS" list in the same commit that ships it.

## Re-measure

```
cd /opt/swift/apps/CoreSwift-CRM
ls src/private_email/*.rs | wc -l                    # 12 + providers/{mailgun,smtp}.rs
grep -ril 'webmail'          src/ | wc -l            # 0
grep -ril 'caldav\|carddav'  src/ | wc -l            # 0
grep -rn 'private_email_messages' migrations/ src/ | wc -l   # 0
```
