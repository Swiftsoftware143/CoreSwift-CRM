# Helpdesk — Phase 0: the base confirmed

Written against the live CoreSwift schema and source (2026-11-14), not against the spec's assumptions.
Every verdict below was read from the database or the code. Where the spec differs from what exists, the
difference is named rather than smoothed over.

## The five assumptions

| # | Assumption in the spec | Verdict |
|---|---|---|
| A1 | A ticket table exists and the private inbox receives mail; extend, do not duplicate | **Holds, and stronger than assumed** |
| A2 | Rust / Axum / Postgres | **Holds**, with two corrections |
| A3 | Contacts and users exist; link, do not duplicate people | **Holds** |
| A4 | Some notion of account, plus a site level under it | **Holds, but the site level must be defined** |
| A5 | The WorkflowSwift AI Router can be called, else use Claude directly | **Simplified — CoreSwift already calls models itself** |

### A1 — the tables exist, and the email↔ticket link is already modelled

Live tables: `tickets`, `ticket_messages`, `ticket_inboxes`, `support_widgets`.

```
tickets          id, tenant_id, subject, description, status, priority, assigned_to,
                 contact_id, source, contact_email, contact_name, widget_id, created_at, updated_at
ticket_messages  id, ticket_id, sender_type, message, created_at
ticket_inboxes   id, tenant_id, name, email_fwd, auto_created, created_at
support_widgets  id, tenant_id, inbox_id, name, slug, theme_color, greeting, welcome_msg,
                 position, is_active, created_at
```

The decisive find is in the private inbox. `private_email_messages` already carries:

```
ticket_id, is_ticket, assigned_user_id, message_id, in_reply_to, provider_message_id, has_attachments
```

So the spec's hardest mechanism — "thread inbound mail onto the right ticket" — has its columns in
place. The plumbing is unbuilt, not unmodelled: nothing currently writes a `ticket_messages` row from
inbound mail, and 17 tickets in the live database are DMARC reports and spam filed by the mail
pipeline, with **zero** messages between them.

The three vocabularies are **enforced by CHECK constraints**, read from the live database:

```
tickets_status_check    open, in_progress, resolved, closed
tickets_priority_check  low, medium, high, urgent
tickets_source_check    manual, email, form, agent, portal
```

Live data: every ticket is `status = open`, `priority = medium`, `source = email`. So the population is
uniform and a vocabulary change is cheap — but it is a **constraint change plus a data change in one
transaction**, never a constraint change alone, or the next write fails on a value the old rows hold.

### A2 — the stack holds; two corrections

`axum 0.7`, `tokio`, `sqlx 0.8`, PostgreSQL **16.14**. Corrections:

1. Route parameters are `:id`, not `{id}`. The booking module arrived written for axum 0.8 and every
   route had to be rewritten.
2. axum's `ws` feature is **off**. Realtime is one line in `Cargo.toml`, but it is a change, not a
   given.

### A3 — people are already one record

`contacts` (26 columns incl. email, phone, first/last name, company, city, state, source, metadata) and
`users` (role, is_platform_admin, is_active). `tickets.contact_id` and `tickets.assigned_to` already
point at them. `hd_contact` becomes a thin extension keyed by `crm_contact_id`; `hd_agent` a thin seat
keyed by `user_id`.

### A4 — accounts exist; the site level does not, and that is the one real design decision

There is a `workspaces` table (name, is_default, tenant_id) — and **no other table references it**. No
table carries `workspace_id`. Today the only isolation boundary in the product is `tenant_id`.

Every one of David's brands is already its own tenant: `swiftsoftware`, `swiftimpact-solutions`,
`giraudy-capital`, `zaarhub`, `bvpp`, `xride-drivers`.

**Recommendation: for Phase 1 the spec's `site_id` maps to `tenant_id`.** `hd_site` becomes a
configuration row keyed by tenant (brand, support address, allowed origins, AI settings). Every
helpdesk query is scoped by `tenant_id` exactly like the rest of the app, so isolation inherits a
boundary that is already proved. A second brand under one account later becomes an added column, not a
rewrite. Inventing a second isolation dimension now would mean re-proving tenancy across a table set
that has never carried one.

### A5 — no dependency on the AI Router

`src/ai/router.rs` already speaks `https://api.anthropic.com/v1/messages` and
`https://api.openai.com/v1/chat/completions`, with `RouterAi`-style selection of provider, model and
key inside CoreSwift. The helpdesk calls that. `DirectClaudeAi` is therefore not a fallback to build —
it is what exists, behind the name the spec wants.

## Table mapping

`account_id` → `tenant_id`. `site_id` → `tenant_id` for Phase 1 (see A4).

| Spec table | Where it lands | Notes |
|---|---|---|
| `hd_site` | **new** `helpdesk_sites` | one row per tenant: brand, support_email, allowed_origins, ai settings, default queue |
| `hd_widget_config` | **extend** `support_widgets` | add public_key, mode, require_email, show_kb, upload limits, offline_message, allowed origins |
| `hd_contact` | **extend** `contacts` | locale, verified, last_seen_at added; identity stays in `contacts` |
| `hd_agent` | **extend** `users` | seat fields: display_name, skills, max_open, presence, site list |
| `hd_queue` | **new** `helpdesk_queues` | members, round_robin_cursor |
| `hd_ticket` | **extend** `tickets` | see field map below |
| `hd_message` | **extend** `ticket_messages` | see field map below |
| `hd_attachment` | **new** `helpdesk_attachments` | none exist today; `has_attachments` is only a flag |
| `hd_tag` | **reuse** `tags` | already 338 rows, with `tag_categories` |
| `hd_sla_policy` | **new** | |
| `hd_kb_category` / `hd_kb_article` | **new** | no knowledge tables exist |
| `hd_kb_chunk` | **new** | `embedding` nullable; `tsv` populated from the start |
| `hd_canned_reply` | **new** | |
| `hd_automation` | **new** | `automation_rules` exists but is empty and shaped differently |
| `hd_csat` | **new** | |
| `hd_chat_session` | **new** | |
| `hd_audit_log` | **extend** `audit_logs` | already exists with 5 rows |
| `hd_webhook` | **reuse** `webhook_endpoints` | exists, empty; events and secret per site |
| `hd_ai_log` | **new** | |

### `tickets` field map

Note: widening a CHECK constraint and updating the rows it governs must ship in ONE migration, or the
rows written under the old vocabulary violate the new constraint the moment it is added.

| Spec | Existing | Action |
|---|---|---|
| `number` per site | — | **add** `number` + `helpdesk_ticket_counters` (per-site counter in the same transaction) |
| `status` | `status` (open/in_progress/resolved/closed) | widen to `new/open/pending/on_hold/solved/closed`, mapping in the same migration: `in_progress → open`, `resolved → solved`, `open`/`closed` unchanged |
| `priority` | `priority` (low/medium/high/urgent) | **keep as is.** The spec says `normal`; the live constraint says `medium`. Mapping `normal → medium` keeps the existing rows and every current writer valid — renaming would break them for no gain |
| `channel` | `source` (manual/email/form/agent/portal) | add `widget` and `api` to the constraint; keep the rest. `manual`/`agent` stay as the "an agent opened it" values, `portal` is the hosted form. Reporting groups manual+agent as agent-created rather than pretending they are a channel |
| `assignee_id` | `assigned_to` | keep |
| `queue_id`, `tags[]`, `ai_state`, `sla_policy_id`, `first_response_due`, `resolution_due`, `first_responded_at`, `solved_at`, `merged_into_id`, `csat_score`, `custom_fields` | — | **add** (`tags` as a link table to `tags`, not a text array) |
| `contact_id` | `contact_id` | keep |

### `ticket_messages` field map

| Spec | Existing | Action |
|---|---|---|
| `author_type` | `sender_type` | keep column; widen `agent/contact` to `contact/agent/ai/system` |
| `kind` (reply/note/event) | — | **add** — this is what makes an internal note possible |
| `body_html` / `body_text` | `message` | **add** both; keep `message` as the text body |
| `email_message_id`, `in_reply_to`, `references[]` | — | **add** — for threading outbound and matching inbound |
| `ai_confidence`, `ai_sources` | — | **add** |
| `edit_of` | — | **add** (append-only with soft edits) |

## Deliberate deviations from the spec

1. **`site_id` = `tenant_id` in Phase 1** (A4). Revisit only when an account genuinely runs two brands.
2. **pgvector is not installable today without changing the database image.** Confirmed: `vector` is
   absent, `pg_trgm` absent, and PostgreSQL runs as an Alpine/musl container. The spec allows the
   fallback, so the knowledge base ships on `tsv` full-text from day one and `embedding` stays null.
3. **No Cloudflare R2 credentials exist on this box.** Attachments go to local disk behind nginx with
   signed, short-lived URLs; `storage_key` is written so R2 is a storage swap, not a schema change.
4. **Preact, not React/TypeScript/Tailwind.** CoreSwift's console is one served file in Preact. The agent
   workspace is built in that style; a second framework would be a second build to keep alive.
5. **The worker runs inside the app process**, not as a second process. `tokio-cron-scheduler` is
   already a dependency; the job table is `FOR UPDATE SKIP LOCKED` as specified.
6. **UUIDv4, not v7.** Every table defaults to `gen_random_uuid()`. Sortable ids would mean changing the
   convention across the product; ordering by `created_at` gives the same result. New helpdesk tables
   will use v4 for consistency and carry an index on `created_at`.

## Answers needed before Phase 1 code

1. **Part 2 of the spec** — data model, API contracts and screens are here; the field-level mapping
   above is my proposal for how they land. Confirm or correct.
2. **The 17 existing tickets are DMARC reports and spam.** Proposal: mark them `status = closed`,
   `channel = email`, tags `automated-report`, and hide them from default views. Nothing is deleted.
3. **What the AI may never do:** my default is answer, suggest and escalate — never close a ticket,
   never promise a refund or a deadline, never delete anything.

## Phase 1 checklist, against what already exists

| Spec Phase 1 item | Baseline |
|---|---|
| Sites with brand, support address, allowed origins | new; one row per tenant |
| Ticket create / reply / internal note / status / priority / tags / assign | **exists** except internal note (needs `kind`) |
| Email in, threading by token and headers | columns exist, plumbing missing |
| Email out with correct headers per site | outbound path exists; header support missing (`Reply-To` is stored but unread) |
| Agent workspace, views, ticket screen, canned replies | console screens exist for list/open/reply; views and canned replies new |
| Widget v1 bubble + inline, theming, chat to ticket | `support_widgets` exists; a booking-style embed script is the pattern to follow |
| Help centre articles, KB suggestions | new |
| AI v1 with citations, handoff, confidence | model calls exist; retrieval and rules new |
| Basic API + `ticket.created` / `message.created` webhooks | ticket API exists; webhooks endpoint table exists |

---

## What is BUILT (updated 2026-11-14, after migrations 162–169)

This section is the reality check on the plan above. If the two disagree, this one is right.

| Piece | State | How it is proved |
|---|---|---|
| Ticket numbering per account | built (162) | trigger allocates; numbers unique per account |
| Status/priority/channel vocabularies | widened (162) | CHECK constraints read from the live DB |
| Internal notes (`kind`) | built | 12/12 — a note never reaches the portal, the count, or the public lookup |
| First response + resolution clocks | built (163) | 13/13 views; 7/7 worker |
| One conversation, one ticket | built (`helpdesk::ingest`) | 12/12 through the signed webhook |
| Machine mail kept out of the queue | built (`helpdesk::junk`) | DMARC report and auto-reply create nothing |
| Ticket views (`?view=`) | built | 13/13, incl. another account's ticket absent from every view |
| SLA warnings and breaches | built (165, `helpdesk::sla`) | 7/7 against the RUNNING worker, no duplicates on a second tick |
| Assignment to a teammate | fixed (164 — pointed at contacts) | a user is accepted, a contact refused, guard proved |
| Merge and split | built (`helpdesk::merge`) | 18/18, including every refusal |
| Knowledge base + search | built (166, `helpdesk::kb`) | 12/12, attacking the public endpoint with the internal article's own words |
| Widget article search | built | proved in a real browser on a fake customer site |
| Saved answers with placeholders | built (167, `helpdesk::canned`) | 12/12, render on the server |
| Desk report | built (`helpdesk::reports`) | 12/12, incl. "a dash, not a zero" |
| Attachments on the timeline | built (168, `helpdesk::attachments`) | 7/7, file only on the ticket its mail belongs to |
| Customer history in the ticket | built | 6/6, incl. the same address in two accounts |
| **AI answers** | **NOT built** | needs a model key in the Integration Center |

### Deliberate decisions that differ from the spec

1. `site_id` = `tenant_id`. The spec's site level does not exist in CoreSwift and inventing it would mean
   re-proving tenancy across tables that have never carried a second dimension.
2. No `embedding` column and no vector search: pgvector is not installable here. Search is full-text, and the
   column simply does not exist rather than sitting empty.
3. Clocks are wall-clock. Business hours and pause-while-pending are not applied to the arithmetic yet, and
   the report says so rather than pretending.
4. Attachments are linked, not duplicated: the bytes stay where the mail pipeline put them.
5. Ticket numbers, SLA stamping and attachment links are triggers/columns, not code paths, because the code
   that creates a ticket is five paths today and more coming.
