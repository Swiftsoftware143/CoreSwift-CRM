# One app, one slug: the names CoreSwift answers to

CoreSwift is called four different things across the fleet, and every one of them is correct in the
layer that uses it:

| where | spelling |
|---|---|
| this repository, and the pre-push `$APP` | `CoreSwift-CRM` |
| the writer registry's lock name | `coreswift-crm` |
| the **deploy key, container and image tag** | `coreswiftcrm` — **canonical** |
| the pre-build gate | `coreswift` |

That is fine for machines that each use one of them, and it is a trap for anybody who compares two:
a pre-push arm written as `[ "$APP" = "coreswiftcrm" ]` matched none of the spellings the hook
actually passes, so the arm ran nothing and the push printed no line from it — a gate that looked
present and protected nothing (measured 2026-11-21).

The rule, therefore:

- **Resolve, never compare.** `/opt/swift/fleet/app-slug.py <any spelling>` prints the canonical slug,
  and `--check <a> <b>` exits 0 when two spellings mean the same app. It ignores case and every
  non-alphanumeric character, so `CoreSwift-CRM`, `coreswift-crm`, `coreswiftcrm`, `coreswift`,
  `crm-swift` and `CORESWIFTCRM` are one name.
- `bin/deploy-app.sh` accepts every spelling too, and says which slug it resolved to, so nobody has to
  read the table to ship.
- When a new gate or script needs the app name, call the resolver. A literal string comparison against
  one spelling is how the silent-failure class starts.

Nothing is renamed on disk: the repository directory, the container and the image tag all stay as they
are. Only the *resolution* is centralised.
