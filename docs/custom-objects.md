# Custom Objects — the owned no-code backend

**Status:** Phase 1 shipped, plus object archive and record purge. **Module:** `src/custom_objects/` ·
**Migrations:** `204_custom_objects.sql`, `206_custom_objects_archive.sql`
**Replaces:** Bolt.new + NoCodeBackend (neither had a workload; nothing was migrated).

## What it is

Any MCP-capable editor — Cursor, Claude, an agent of ours — can describe a data shape and get it:
objects, fields and record endpoints, living in this database, scoped to one workspace. Bolt and
NoCodeBackend sold that provisioning step and nothing else we were missing; hosting, Postgres,
tenancy, key minting and MCP we already ran.

It is a **module of CoreSwift-CRM**: no new app, no new database, no new deploy path.

## Two doors, one implementation

| Door | Address | Credential |
|---|---|---|
| MCP (JSON-RPC 2.0 over HTTP) | `POST /api/mcp` | `Authorization: Bearer csk_…` (issued key) or a console session token |
| REST | `/api/objects/**` | same two |

Both credential classes resolve to exactly **one account** in `custom_objects::authenticate`; both
doors call the same functions in `src/custom_objects/store.rs`. Validation, uniqueness and audit
therefore cannot drift between what an agent writes and what an application writes.

`GET /api/mcp` answers a small JSON index (service name and both tool lists — the Custom Objects
tools and the CRM's domain reads) for a human with `curl`. It carries no account and no data: the
POST is the authenticated door.

### REST surface

```
GET    /api/objects                      list objects + field/record counts
POST   /api/objects                      {slug,name}
DELETE /api/objects/{slug}               archive the object (fields too; records kept) — idempotent
PATCH  /api/objects/{slug}               {unique_together:[...]}  set/clear the composite key
GET    /api/objects/{slug}/schema        object, live fields, archived fields, record count, addresses
POST   /api/objects/{slug}/fields        {key,label,kind,required,unique,options,position}
PATCH  /api/objects/{slug}/fields/{id}   rename / label / position / required / unique / options
DELETE /api/objects/{slug}/fields/{id}   archive (the data stays)
GET    /api/objects/{slug}/records       ?limit&offset&sort&filter.<field>=<value>
POST   /api/objects/{slug}/records       the record's own fields at the top level
POST   /api/objects/{slug}/records?upsert=k1,k2   replay-safe: insert or update that key
PATCH  /api/objects/{slug}/records/{id}  merge; only the keys you send change
DELETE /api/objects/{slug}/records/{id}  soft delete (deleted_at); ?hard=true deletes the row
GET    /api/objects/{slug}/audit         ?limit  the schema-change trail
```

A record body is **the record itself** — `{"email":"a@b.c"}` — not wrapped in `data`. (MCP's
`insert_record` does take a `data` argument; sending the wrapped shape to REST answers 400 with a
sentence that says so.)

### MCP tools (11)

`list_objects`, `create_object`, `describe_object`, `archive_object`, `add_field`, `archive_field`,
`insert_record` (optional `upsert_on`), `set_unique_together`, `query_records`, `update_record`,
`delete_record`.

### Domain reads (12) — the CRM's own data on the same door

Card `t_e9ab0027` put the CRM behind the same `POST /api/mcp`: the transport, the credential contract,
the role gate and the audit line now come from the fleet's shared `mcp-door` crate, and beside the
eleven Custom Objects tools the door offers twelve read-only tools over the app's own tables.

| Tool | Answers |
|---|---|
| `coreswift_list_contacts` / `coreswift_get_contact` | contacts (+ deal/booking/ticket counts on one) |
| `coreswift_list_companies` / `coreswift_get_company` | companies (+ contact/deal counts on one) |
| `coreswift_list_deals` / `coreswift_get_deal` | opportunities — **value WITH its currency**, stage, pipeline, win/loss, recent activity excerpts |
| `coreswift_list_pipelines` | pipelines with their stages in order |
| `coreswift_list_bookings` | bookings, upcoming first, with calendar, resource and the order's customer |
| `coreswift_list_availability` | bookable slots for a date window (`from` required) + the weekly rules |
| `coreswift_list_tickets` | tickets with a 500-char description excerpt (**untrusted customer text**) |
| `coreswift_list_billing` | `tenant_plans` + `plan_lines` — CoreSwift bills through plan lines, there is no invoices table |
| `coreswift_list_messages` | `private_email_messages`, body excerpt capped at 2,000 chars (**untrusted**) |

The names carry the app slug (the door standard) so one agent conversation can hold several products'
doors without ambiguity; the eleven Custom Objects names are frozen because clients already call them.

Every read filters `tenant_id = any(<the token's workspaces>)` **inside the statement**, takes no
workspace argument, and answers `not_found` for another workspace's id. Never projected: the key
tables, the sealed copies, provider keys, `account_erasures`, `audit_logs`.

**A key can be read-only — on BOTH doors.** `POST /api/personal-api-keys
{"name":"…","role":"read_only"}` mints a key that may only read: the MCP door refuses it on every
write tool (Custom Objects included) before the handler runs, and the REST surface refuses it on
every write method with **403**
`{"error":"this key is read-only — use a workspace key to write","status":403}`. The REST read arms it
may use are exactly `GET /api/objects`, `GET /api/objects/:slug/schema`, `GET /api/objects/:slug/records`,
`GET /api/objects/:slug/audit` and `GET /api/external/lists`; everything else on the issued-key
surface — create/patch/archive object, field add/patch/archive, record insert/update/delete,
`/api/external/contacts`, `/api/external/deals` and its stage/status arms, and the data-mapping
intake — answers that 403 and writes nothing. The role is enforced once in the app-wide credential
boundary (`src/auth/boundary.rs`) from the committed allowlist
(`auth::route_policy::READ_ONLY_KEY_ROUTES`), so a route added later is refused until it is listed.
A `workspace` key is unaffected. `GET /api/personal-api-keys` shows each key's `role`. Operator
authority is not a key role — a platform admin's key resolves to the operator role from
`users.is_platform_admin`.

**Audit.** One JSON line per call at `/app/data/mcp-audit.log` (host: `/opt/swift/coreswiftcrm-data`,
mode 0600), written before the response, arguments redacted and truncated, subject = the key's id or
the user's id — never the token. A refused call writes a line too (`unauthorized: <reason>`).

**Proof.** `python3 scripts/mcp-door-acceptance.py` (25 checks: two probe workspaces, foreign id →
`not_found` while its owner reads it, differing counts, read-only refused on writes, revoked key 401,
`door-contract.py` 14/14, audit mode/delta/token-scan), `python3
scripts/custom-objects-acceptance.py` (33 checks — the eleven tools unchanged through the new
transport) and `python3 scripts/api-key-role-acceptance.py` (the REST half of `read_only`: the read
arms answer 200, every write arm on both surfaces answers 403 with the sentence, the MCP door is
unchanged, a workspace key still writes, and the probe workspace leaves no residue).

No tool takes a workspace or tenant argument. An id belonging to another account answers 404 (never
403 — a refusal must not confirm the row exists). Unknown arguments are refused by name, so a typo
cannot silently perform a different operation.

### Composite keys, and writes that can be replayed

A single field can already be `unique`. A **combination** of fields is `unique_together`: the list of
field keys whose values together must be unique among live records — the shape a producer needs when
one booking has many positions (`booking_ref` + `position`).

```bash
# once, per object
curl -X PATCH https://coreswiftcrm.com/api/objects/ad_fulfillment \
  -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"unique_together":["booking_ref","position"]}'

# every write is now replay-safe: same key values -> the SAME row, fields merged
curl -X POST 'https://coreswiftcrm.com/api/objects/ad_fulfillment/records?upsert=booking_ref,position' \
  -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"booking_ref":"b-1","position":"leaderboard","city":"Apopka","issue_date":"2026-11-24"}'
```

Rules the writer enforces:

- **Only fields in `unique_together` can key an upsert.** Sending any other set is a 400 that names
  both lists; the caller must set the key first, not invent one per request.
- **A record missing any key value is not keyed at all** (it will not collide), so an upsert must
  carry all of them — otherwise 400.
- **The constraint holds outside the upsert door.** A plain insert (or a PATCH) that would duplicate
  an existing combination answers 409 and writes nothing; the message suggests the `?upsert=` form.
- **It is refused while live rows already collide**, and the refusal names the colliding values and
  the row ids (`booking_ref='b-1', position='leaderboard' — 2 live rows (ids …)`). Resolve the
  duplicate, then set the key. Deleting or overwriting a customer's row to satisfy a schema change is
  never something the writer does on its own.
- **Existing rows are re-keyed in the same transaction**, so an object with data obeys the constraint
  immediately instead of only for rows written afterwards.
- Archiving (`deleted_at`) a row frees the combination again; a hard delete frees it too.

Audit: each successful write of this kind records `record_upserted`, the successful set records
`unique_together_set`, and both carry the record/field ids only — never the data.

### Undoing what you created

- **`archive_object` / `DELETE /api/objects/{slug}`** is the only way to remove an object. The object
  and its live fields get `archived_at`; it disappears from `list_objects`, `describe_object` and
  every record/field/audit route (404), and its **records are kept** — still in `custom_records`,
  readable by record id from support/SQL. Idempotent: a retry answers `already_archived: true`.
  The slug becomes available again for a new object. There is **no** object hard delete.
- **`delete_record` / `DELETE …/records/{id}`** is a soft delete by default. Add `hard: true` /
  `?hard=true` to actually delete the row and its data; it also purges a row that was soft-deleted
  earlier. The audit trail records `record_purged` with the **id only**, never the data.
- **No API key and no MCP tool reaches account-level erasure.** That door
  (`POST /api/auth/erase-data`) is a signed-in human's alone; a key gets 401 and an `erase_data` tool
  name is `-32601 unknown tool`.

## The rules the code enforces

- **The tenant comes from the credential, never from the request.** No body, query or path field is
  read for it.
- **Field kinds** `text | number | bool | date | enum | json`; a value of the wrong kind is a 400
  that names the field and the kind. `enum` needs `options` and its value must be one of them.
- **Unknown keys are refused** on insert and patch — no silent schema drift, and nothing is written.
- **Required** is checked on insert and on the MERGED result of a patch, so a patch cannot drop a
  required value by omitting it.
- **`unique`** is enforced inside a transaction holding `pg_advisory_xact_lock` on the object, so two
  concurrent inserts cannot both pass a read-then-write check. A violation answers 409 and no row is
  written.
- **Archiving is the only schema deletion.** `archived_at` hides a field from writes and from
  `fields`, keeps its values in `custom_records.data`, and re-adding the same key revives it. An
  object archive takes its live fields with it and leaves every record row in place.
- **Soft delete by default, hard delete on request** (`hard: true`); a soft-deleted row is never
  returned by list/query, and a hard delete keeps only the id in the audit trail.
- **Renaming a field key with live data is refused** (409 with the count) — renaming would orphan
  values. Archive + add is the Phase 1 migration path.
- **No client-supplied SQL**, no DDL from tenant input, no per-tenant schemas.

## Scope: what `workspace_id` means here

The spec says `workspace_id`; the column is `tenant_id`. A personal API key resolves to exactly one
account (`personal_api_keys.tenant_id`) and a machine caller has no `workspaces.id` at all, so the
**account** is the only unit both credential classes can agree on. `workspaces` rows are a sub-scope
of the same account and are deliberately not used here.

## Proving it

```bash
python3 scripts/custom-objects-acceptance.py                    # against 127.0.0.1:8084
python3 scripts/custom-objects-acceptance.py --base https://coreswiftcrm.com
```

Thirty-three checks covering the spec's eight, the two exit doors, and composite keys + upsert: MCP
create + fields, REST insert with a key, console-session read of the same row, a second workspace's
key answering 404, archiving a field keeping data, an unknown key answering 400 with nothing written,
a duplicate unique value answering 409, audit rows for every schema change, object archive (gone from
list, fields archived, records kept, idempotent, other workspace 404, slug reusable), record
hard-delete (one row gone, siblings kept, audit carries the id only), soft delete unchanged, the MCP
arms of both, no key/MCP path to account-level erase, the composite key being set through PATCH and
audited, one-upsert-inserts / replay-updates (same id, merged fields, still one row), a second
position making two rows with replays leaving two, the plain-insert 409 outside the upsert door, the
second workspace 404 on the key and the upsert door, the unknown-field and wrong-key-set 400s, the
duplicate refusal naming the values and both ids, the re-key after the duplicate is removed, the MCP
upsert, both refusal doors (no credential, unknown key), and its own cleanup — it creates two marked
probe tenants (`probe_harness = 'custom-objects-acceptance'`), then deletes them and asserts the
baseline came back.

## Out of Phase 1 (deliberately)

End-user auth for the customer's own app, relations/joins/rollups, a per-policy RLS editor, and
file/media storage (Engine's library already does that — do not duplicate it). Console schema designer
and record browser are Phase 3; the MCP and REST surfaces are the interface until then.
