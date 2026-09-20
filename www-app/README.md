# www-app/ — the tracked source of truth for app.coreswiftcrm.com

This directory is the **tenant-facing app surface** of CoreSwift CRM. nginx serves it
from `/opt/swift/nginx/www-app/coreswift/` (see `sites/coreswift.conf`, the
`app.coreswiftcrm.com` server block, which proxies `/api/` to `127.0.0.1:8084`).

Nothing here is generated at runtime: **edit these files, then publish them.**

## Publish (this is the only sanctioned way)

```bash
/opt/swift/bin/deploy-coreswift-app.sh          # syntax gate -> copy -> md5 verify -> HTTP smoke
/opt/swift/bin/deploy-coreswift-app.sh --dry-run  # list what would be published
```

The script refuses to publish if any inline `<script>` fails `node --check`, then verifies
every published file is byte-identical (`md5sum`) to this directory, then smoke-tests
`/`, `/login`, `/dashboard/`, `/register/` over the real domain.

A repo-only edit is invisible until the script runs — nginx serves real files, not a symlink.

## Files

| File | Route |
|------|-------|
| `index.html` | `/` — the dashboard SPA (`#/overview`, `#/contacts`, `#/pipelines`, `#/tickets`, `#/integrations`, `#/plan`) |
| `login.html` | `/login` — sign-in + register tabs; the single entry point for tenants |
| `register/index.html` | `/register/` — redirect stub to `/login` |
| `dashboard/index.html` | `/dashboard/` — redirect stub to `/` |

## What this replaced

`public/index.html` used to live here in spirit: it was **six whole page copies
concatenated** (one `<script>` open, six `</script>` closes) with an unterminated string
literal at line 859, so its JavaScript could not parse at all. It was also never served —
the container runs with `WorkingDir /app`, which only contains `crm-swift` and `migrations`,
so `ServeDir::new("public")` answered **404** on `127.0.0.1:8084/`. It was deleted;
`git show <this-commit>^:public/index.html` recovers it if anyone ever wants it.

The Integrations view (`#/integrations`) is wired to the hub's live endpoints:
`GET /api/integration-center/overview` (and `/lead-sources`), `POST|DELETE /api/personal-api-keys`,
`POST /api/personal-api-keys/:id/rotate`.
