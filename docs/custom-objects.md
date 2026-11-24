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

`GET /api/mcp` answers a small JSON description (service name, tool list, the account the token acts
as) — handy for a human with `curl`, useless to anyone without a credential.

### REST surface

```
GET    /api/objects                      list objects + field/record counts
POST   /api/objects                      {slug,name}
DELETE /api/objects/{slug}               archive the object (fields too; records kept) — idempotent
GET    /api/objects/{slug}/schema        object, live fields, archived fields, record count, addresses
POST   /api/objects/{slug}/fields        {key,label,kind,required,unique,options,position}
PATCH  /api/objects/{slug}/fields/{id}   rename / label / position / required / unique / options
DELETE /api/objects/{slug}/fields/{id}   archive (the data stays)
GET    /api/objects/{slug}/records       ?limit&offset&sort&filter.<field>=<value>
POST   /api/objects/{slug}/records       the record's own fields at the top level
PATCH  /api/objects/{slug}/records/{id}  merge; only the keys you send change
DELETE /api/objects/{slug}/records/{id}  soft delete (deleted_at); ?hard=true deletes the row
GET    /api/objects/{slug}/audit         ?limit  the schema-change trail
```

A record body is **the record itself** — `{"email":"a@b.c"}` — not wrapped in `data`. (MCP's
`insert_record` does take a `data` argument; sending the wrapped shape to REST answers 400 with a
sentence that says so.)

### MCP tools (10)

`list_objects`, `create_object`, `describe_object`, `archive_object`, `add_field`, `archive_field`,
`insert_record`, `query_records`, `update_record`, `delete_record`.

No tool takes a workspace or tenant argument. An id belonging to another account answers 404 (never
403 — a refusal must not confirm the row exists). Unknown arguments are refused by name, so a typo
cannot silently perform a different operation.

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

Twenty-two checks covering the spec's eight plus the two exit doors: MCP create + fields, REST insert
with a key, console-session read of the same row, a second workspace's key answering 404, archiving a
field keeping data, an unknown key answering 400 with nothing written, a duplicate unique value
answering 409, audit rows for every schema change, object archive (gone from list, fields archived,
records kept, idempotent, other workspace 404, slug reusable), record hard-delete (one row gone,
siblings kept, audit carries the id only), soft delete unchanged, the MCP arms of both, no key/MCP
path to account-level erase, both refusal doors (no credential, unknown key), and its own cleanup —
it creates two marked probe tenants (`probe_harness = 'custom-objects-acceptance'`), then deletes
them and asserts the baseline came back.

## Out of Phase 1 (deliberately)

End-user auth for the customer's own app, relations/joins/rollups, a per-policy RLS editor, and
file/media storage (Engine's library already does that — do not duplicate it). Console schema designer
and record browser are Phase 3; the MCP and REST surfaces are the interface until then.
