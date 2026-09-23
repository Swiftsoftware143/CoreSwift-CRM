# The CoreSwift-CRM marketing root: `public/` -> `/opt/swift/nginx/www/coreswift`

Decided by **t_6091c584** (fleet decision card: **t_8f957ca6**, doc `/opt/swift/docs/fleet-marketing-www.md`).

## The three layers

| layer | path | role |
|---|---|---|
| authoring | `apps/CoreSwift-CRM/public/` (this repo) | source of truth, `gate` mode in the parity table |
| deploy record | `/opt/swift/nginx` (frontends repo) | every served file must have a commit here |
| live | `/opt/swift/nginx/www/coreswift` | served by the `coreswiftcrm.com` vhost; hand-`cp` has no publisher script, so the gate is the sanctioned path |

```bash
python3 /opt/swift/fleet/marketing-www-parity.py --audit --app CoreSwift-CRM
python3 /opt/swift/fleet/marketing-www-parity.py --publish CoreSwift-CRM <rel>...   # repo -> served
python3 /opt/swift/fleet/marketing-www-parity.py --record  CoreSwift-CRM <rel>...   # served -> frontends
```

## What `public/` actually holds — two different classes

**1. Marketing-root pages** — served byte-for-byte out of `www/coreswift`:
`index.html`, `privacy.html`, `refunds.html`, `terms.html`, `sitemap.xml`, `robots.txt`,
`admin-guide.html`, `guide.html`, `favicon.ico`, `favicon.svg`, `favicon-d6446083.{ico,svg}`,
`assets/og-coreswift.{png,svg}`.

**2. Binary-embedded assets — NOT nginx files, and they must not be moved out:**
`support-portal.html` is compiled into the service
(`const PORTAL_HTML: &str = include_str!("../../public/support-portal.html")`,
`src/tickets/portal.rs:50`) and served by the app itself at
`GET /s/:tenant_id/support`, which both the marketing and the app vhost proxy through
`location /s/`. Deleting or relocating it breaks the build, not just a page.

The app's own `ServeDir::new("public")` (`src/main.rs:349`) is empty in production: the
Dockerfile copies only `crm-swift`, `migrations` and `.env.example` into `/app`. So the binary
serves nothing from `public/` except the `include_str!` assets above.

## Pitfall: the mode of a repo file becomes the mode of the served file

`--publish` used `shutil.copy2`, which preserves the source mode. `public/sitemap.xml`,
`public/robots.txt` and `public/guide.html` still carried the legacy `0750 root:swift`, so a
published file landed unreadable by the `www-data` nginx worker: the vhost answered **HTTP 403
while the gate printed `OK md5 parity`** (measured live on `/sitemap.xml`, 2026-09-22). The gate
now `chmod 0644`s the destination (guard repo commit `6eb25a2`, asserted in
`fleet/tests/test_marketing_www_parity.sh`), and the tracked modes in this root were normalised to
`0644` so a hand-`cp` cannot reintroduce it.

## The five `ABSENT-LIVE` rows t_6091c584 decided

Measured against all three live vhosts (`coreswiftcrm.com`, `app.coreswiftcrm.com`,
`admin.coreswiftcrm.com`) and against every reference in the served roots and in this repo.

| file | verdict | evidence |
|---|---|---|
| `sitemap.xml` | **served** | the served `robots.txt` advertises `Sitemap: https://coreswiftcrm.com/sitemap.xml` and that URL answered 404. Published through the gate: live 200, 233B, md5 `1e8d54abe9ae`, recorded in the frontends repo (`2cf0d75`). |
| `admin-login.html` | removed | `location = /admin-login.html` is a deliberate `301 /login` on the app vhost and `301 /` on the admin vhost; the file is served by no root, and nothing in any served page or in this repo references it. |
| `login.html` | removed | one login URL by design: `app.coreswiftcrm.com/login` serves `www-app/coreswift/login.html` (9017B, tracked in this repo); the marketing vhost has no `/login` route. `public/login.html` (10444B) is a stale second copy served nowhere, referenced nowhere. |
| `inline-guide-widget.js` | removed | zero references in any served root; the guide's embed is the per-tenant script the app serves at `/s/<tenant-id>/widget.js` (see the `location /s/` comment in `sites/coreswift.conf`). |
| `thank-you.html` | removed | zero references anywhere; `/thank-you.html` on the app vhost is answered by the SPA fallback (`try_files ... /index.html`), never by this file. |

The four removed files stay recoverable from git history by blob:

```bash
git show b2aa4ae5b9bd   # public/admin-login.html   3870B
git show 8de4bf37a847   # public/login.html        10444B
git show 6ac603ef4fc9   # public/inline-guide-widget.js 8752B
git show 18412d836f7c   # public/thank-you.html     3488B
```

Kept, with the reason recorded above: `support-portal.html` (compile-time dependency + live at
`/s/:tenant_id/support`). `--audit` therefore still prints one `ABSENT-LIVE` row for it — that row
is informational, not a hazard: it is not in the gate's `HAZARDS` set and never trips the
`*/30` `marketing-www-watch.sh` guard.
