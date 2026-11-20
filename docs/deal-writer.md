# The Deal Writer — the contract for an outside system

_Shipped 2026-11-20 (kanban t_2df68850). Read this before writing a client._

CoreSwift's deal record is the `opportunities` table (pipeline + stage + value + history). The deal
writer is the ONE function that writes it (`src/deals/writer.rs::upsert`), reached by two doors: the
console (`/api/deals`) and an issued API key (`/api/external/deals`). A client never needs to know
which pipeline, stage row or contact id to use — it names them, and the writer resolves or creates
them inside the caller's own workspace.

## Auth

`Authorization: Bearer <personal API key>`. The key resolves the workspace; a `tenant_id` / `aid` in
the body is **ignored** (it is not a credential). Keys are minted in the workspace's Integration
Center and can only be revealed by that workspace's own signed-in owner.

Every call is metered against the plan's daily API-call ceiling (402 when it is reached).

## The one rule: `external_ref`

`external_ref` is **required** on every deal push and is the deal's identity inside the workspace.

* first push with a new reference → `201 {"created": true, ...}`
* every later push with the same reference → `200 {"created": false, ...}`, updating that deal

Uniqueness is a database index on `(tenant_id, external_ref)` (partial, so console-created deals
need none), which means it holds even when two pushes race. Two different workspaces may use the
same reference — they are different deals.

XRide Dispatch sends its **quote id** here. An unbooked quote becomes a deal; a re-send updates the
same deal.

## Endpoints

```http
POST  /api/external/deals
      { "external_ref": "QUOTE-1042", "title": "Airport transfer", "value": 240.00,
        "currency": "USD", "stage": "Qualified", "contact_email": "rider@example.com",
        "contact_name": "Ada Rider", "expected_close_date": "2026-12-01",
        "source": "xride-dispatch", "status": "open",
        "custom_fields": { "service_level": "Priority", "pickup_city": "Miami" } }

PATCH /api/external/deals/:external_ref/stage    { "stage": "Proposal" }   or  { "stage_id": "<uuid>" }
POST  /api/external/deals/:external_ref/status   { "status": "won" }       or  { "status": "lost", "lost_reason": "Price" }
```

Accepted fields on `POST /api/external/deals`:

| field | notes |
|---|---|
| `external_ref` | **required**, the dedupe key |
| `title` / `name` | the deal's name (either spelling) |
| `value` | number; `null` clears it |
| `currency` | 3-letter code. Falls back to the workspace's own currency. **Never assumed to be USD** — a workspace with no currency is refused with a sentence naming both remedies |
| `exchange_rate` | optional; stored WITH the deal (and stamped `rate_date`) so a past period never moves when rates do |
| `probability` | 0-100. Omitted → the stage's own default |
| `expected_close_date` | `YYYY-MM-DD` |
| `pipeline_id` / `pipeline` | id, or a name. Omitted → the workspace's default pipeline (created on first use, with stages) |
| `stage_id` / `stage` | id, or a name on that pipeline. Omitted on create → the first stage |
| `contact_id` / `contact_email` + `contact_name` + `contact_phone` | an email find-or-creates the contact (the only field with a uniqueness guard) |
| `company_id` / `company_name` | a name find-or-creates the company |
| `owner_id` / `owner_email` | must be a user of this workspace |
| `collaborator_ids` | array of user ids |
| `source` | where the deal came from (`xride-dispatch`) |
| `notes` | free text |
| `status` | `open` \| `won` \| `lost`; won/lost also move the deal to the pipeline's closed stage |
| `lost_reason` | free text (the console offers the workspace's configured list) |
| `custom_fields` | object. Each key becomes a per-workspace field on first use |
| `metadata` | jsonb, untouched by the app |

Responses always say which arm ran:

```json
{ "id": "…", "external_ref": "QUOTE-1042", "created": true, "matched_by": "external_ref",
  "stage_moved": false, "pipeline_created": true, "title": "…", "pipeline_id": "…",
  "stage_id": "…", "value": 240.0, "currency": "USD", "probability": 30, "status": "open" }
```

## Custom fields = the industry layer

Nothing industry-specific is in the core. Xride's courier fields live in the workspace's own
`deal_custom_fields`:

| key | label | type |
|---|---|---|
| `service_level` | Service level | text |
| `pickup_city` | Pickup city | text |
| `dropoff_city` | Drop-off city | text |
| `job_date` | Job date | date |
| `quote_reference` | Quote reference | text |

Send them under `custom_fields` and they are created and written in one call. The console shows them
in the deal's detail panel, and a workspace admin can add more (or change types) under
Deals → custom fields.

## Money is per currency

The workspace states its own currency once (Deals → Deal writer API → workspace currency, or
`PUT /api/deals/currency`). Every money read then groups by currency: stage totals, the filter
summary, `/api/deals/report` (win rate, average days to close, deals rotting in a stage) and
`/api/pipelines/:id/analytics`. No endpoint adds one currency to another, and no endpoint invents a
currency for a deal that has none.

## Stage hooks

Entering a stage can apply a tag to the deal or run the workspace's `StageChanged` automation rules
(Deals → Stage hooks). The writer fires them on a move, on a status change that lands on a closed
stage, and on a push that lands on a stage — so the outside system cannot move a deal past its
follow-ups.

## Proving a client

`scripts/proof-deal-writer.py` is the acceptance harness: it drives the real HTTP surface with a
signed session token and the workspace's own issued key, asserts the create/re-send/refusals, and
cleans up after itself. Run it in a workspace before wiring a new client:

```bash
python3 scripts/proof-deal-writer.py --tenant-slug xride
```
