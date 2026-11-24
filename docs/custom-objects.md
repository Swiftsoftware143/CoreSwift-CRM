# Custom Objects — the owned no-code backend

**Status:** Phase 1 shipped. **Module:** `src/custom_objects/` · **Migration:** `204_custom_objects.sql`
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

`GET /api/mcp` answers a small JSON description (service name, tool list, the account the token acts
as) — handy for a human with `curl`, useless to anyone without a credential.

### REST surface

```
GET    /api/objects                      list objects + field/record counts
POST   /api/objects                      {slug,name}
GET    /api/objects/{slug}/schema        object, live fields, archived fields, record count, addresses
POST   /api/objects/{slug}/fields        {key,label,kind,required,unique,options,position}
PATCH  /api/objects/{slug}/fields/{id}   rename / label / position / required / unique / options
DELETE /api/objects/{slug}/fields/{id}   archive (the data stays)
GET    /api/objects/{slug}/records       ?limit&offset&sort&filter.<field>=<value>
POST   /api/objects/{slug}/records       the record's own fields at the top level
PATCH  /api/objects/{slug}/records/{id}  merge; only the keys you send change
DELETE /api/objects/{slug}/records/{id}  soft delete (deleted_at)
GET    /api/objects/{slug}/audit         ?limit  the schema-change trail
```

A record body is **the record itself** — `{"email":"a@b.c"}` — not wrapped in `data`. (MCP's
`insert_record` does take a `data` argument; sending the wrapped shape to REST answers 400 with a
sentence that says so.)

### MCP tools (9)

`list_objects`, `create_object`, `describe_object`, `add_field`, `archive_field`, `insert_record`,
`query_records`, `update_record`, `delete_record`.

No tool takes a workspace or tenant argument. An id belonging to another account answers 404 (never
403 — a refusal must not confirm the row exists). Unknown arguments are refused by name, so a typo
cannot silently perform a different operation.

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
  `fields`, keeps its values in `custom_records.data`, and re-adding the same key revives it.
- **Renaming a field key with live data is refused** (409 with the count) — renaming would orphan
  values. Archive + add is the Phase 1 migration path.
- **Soft delete only** for records (`deleted_at`); list/query never returns them.
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

Eleven checks covering the spec's eight: MCP create + fields, REST insert with a key, console-session
read of the same row, a second workspace's key answering 404, archiving keeping data, an unknown key
answering 400 with nothing written, a duplicate unique value answering 409, audit rows for every
schema change, both refusal doors (no credential, unknown key), and its own cleanup — it creates two
marked probe tenants (`probe_harness = 'custom-objects-acceptance'`), then deletes them and asserts
the baseline came back.

## Out of Phase 1 (deliberately)

End-user auth for the customer's own app, relations/joins/rollups, a per-policy RLS editor, and
file/media storage (Engine's library already does that — do not duplicate it). Console schema designer
and record browser are Phase 3; the MCP and REST surfaces are the interface until then.
