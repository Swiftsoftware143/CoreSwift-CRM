# SwiftSoftware Architecture — Single Source of Truth
# Last updated: 2026-09-26
# IF YOU CHANGE ANYTHING BELOW, UPDATE THIS FILE.

## Golden Rules (Read Before Touching ANY App)

### Rule 1: One VPS, One Brain
All 7 apps share this Miami VPS. No app owns the entire machine.
- CARGO_BUILD_JOBS=1 always (2GB RAM constraint)
- One build at a time — check `ps aux | grep rustc` before starting
- If load > 3.0 or memory > 80%, STOP. Tell David.

### Rule 2: FunnelSwift IS the Affiliate Hub
There is ONE affiliate system and it lives in FunnelSwift.
- FunnelSwift owns: affiliate codes, tracking links, clicks, conversions, commissions, payouts
- FunnelSwift admin = affiliate director
- FunnelSwift owns the commissionable catalogue too (`affiliate_products`) — apps do NOT push their
  plans into it (see Rule 4)
- NO other app has independent cross-app commission logic: an app reports a plan upgrade to
  FunnelSwift and that is all

### Rule 3: Two Free Plans in FunnelSwift
| Plan | Slug | Entry Point | What User Gets |
|------|------|-------------|----------------|
| Free | `free` | `app.funnelswift.net/signup` | Full CRM dashboard |
| Kinetic Free | `kinetic_free` | `funnelswift.net/kinetic` (modal) | Bio-link card + lead capture |

### Rule 4: The Commissionable Catalogue Lives in FunnelSwift
FunnelSwift owns `affiliate_products` — one row per commissionable product, tagged with `source_app`
(the app that sells it) — and that table is the source of truth for what an affiliate can promote.
Apps do NOT push their plans into it:
- The receiver for that idea exists — `POST /api/v1/internal/sync-affiliate-plan`
  (`src/api_router.rs`, guarded by the `x-internal-key` header) — but it performs an unconditional
  INSERT and ignores the payload's `action`, so it cannot update or deactivate a product.
- No app has a working caller today (measured 2026-09-26 on the live deployment): CoreSwift does not
  call it at all, and the three senders that do post to it (ADASwift, IncentiveSwift, WorkflowSwift)
  carry the shared key in the payload body (`api_key`) while the route reads the `x-internal-key`
  header, so every such call is refused **401 Invalid internal key** and writes nothing.
- The rows are therefore created inside FunnelSwift: a plan-derived row per FunnelSwift plan, plus one
  active platform-wide row per sibling service seeded by FunnelSwift's own migrations (`source_app` =
  `coreswift`, `adaswift`, `workflowswift`, `incentiveswift`, `missedcallrespondr`).

What apps DO send is the commission trigger: `POST /api/v1/internal/affiliate/upgrade-event`
(`x-internal-key` header) from the app's own plan-change path when a tenant moves onto a paid plan.
That endpoint resolves the product by `source_app` and writes the commission.

### Rule 5: Zaarcash ≠ Affiliate
- **Zaarcash** = loyalty points, owned by IncentiveSwift, used by ZaarHub
- **Affiliate** = commission tracking, owned by FunnelSwift, used by ALL apps
- These are COMPLETELY separate. Never merge them.
- Multi-Directory: has loyalty proxy (Zaarcash) ONLY. No affiliate logic.

## App Directory & Port Map

| App | Path | Port | Service | Domain |
|-----|------|------|---------|--------|
| Multi-Directory | `/opt/swift/multidirectory-rust` | 3001 | multidirectory | directory.swiftsoftware.net |
| CoreSwift CRM | `/opt/swift/coreswift` | 8084 | coreswift-crm | coreswiftcrm.com |
| FunnelSwift | `/opt/swift/funnelswift` | 8080 | funnelswift | funnelswift.net |
| IncentiveSwift | `/opt/swift/incentiveswift` | 8083 | incentiveswift-api | incentiveswift.com |
| WorkflowSwift | `/opt/swift/workflowswift` | 8085 | workflowswift-api | workflowswift.com |
| MissedCall | `/opt/swift/missedcall_respondr` | 8088 | missedcall-respondr | missedcallrespondr.com |
| ADA Swift | `/opt/swift/adaswift` | 8087 | adaswift | adaswift.com |

## Database
All apps share one Postgres instance (Docker: swift-postgres-1).
- Host: 127.0.0.1:5432
- User: swift
- Each app has its own database: `coreswift`, `funnelswift`, `incentiveswift`, etc.

## Each App's Responsibility

### 1. FunnelSwift — The Affiliate Hub
- **AFFILIATE SYSTEM**: Codes, links, tracking, conversions, commissions, payouts
- **Owns**: `affiliate_products`, `affiliate_users`, `affiliate_clicks`, `affiliate_conversions`, `affiliate_links`
- **Two free entry points**: Kinetic modal + standard signup page
- **Commissionable products**: FunnelSwift-owned `affiliate_products` (one row per product, `source_app`
  names the selling app); apps do not push plans into it (Rule 4)

### 2. Multi-Directory — Directory SaaS
- **Zaarcash loyalty proxy** → IncentiveSwift (routes loyalty requests)
- **NO affiliate logic** (was removed, do NOT re-add)
- Serves ZaarHub frontend + multiple tenant directories
- Onboarding survey system for city/preference config

### 3. CoreSwift CRM — CRM Platform
- **Connector to FunnelSwift** via `src/native_apps/connectors/funnelswift.rs` — pushes leads,
  contacts, funnels, tags and product selections (NOT plans)
- **Commission trigger**: `src/billing/handlers.rs` posts `POST /api/v1/internal/affiliate/upgrade-event`
  (`x-internal-key`) when a tenant moves onto a paid plan — CoreSwift's only affiliate write
- **Webhook system** for cross-app events
- **Branch**: `main`
- **Affiliates module** (`/api/affiliates/*`): a tenant-LOCAL programme in this app's own database
  (profile, products, referrals, payouts); it does not sync to FunnelSwift

### 4. IncentiveSwift — Loyalty/Zaarcash Engine
- **OWNS Zaarcash**: points per check-in, credit rate, offers, vouchers, rewards
- **Credit rate config**: per-tenant, defaults to 10 (10 Zaarcash per $1)
- **Plan-sync sender**: `src/handlers/plans_handler.rs` — does not authenticate today (Rule 4)
- **DO NOT touch Zaarcash/loyalty code** when modifying affiliate wiring

### 5. WorkflowSwift — Workflow Automation
- **Plan-sync sender**: `src/handlers/plan_handler.rs` — does not authenticate today (Rule 4)
- n8n integration via `n8n.swiftsoftware.net:5678`
- Affiliates handler is thin CRUD for local table only

### 6. MissedCall Respondr — Missed Call Management
- **Plan-sync sender + checkout conversions** → FunnelSwift; the checkout posts
  `POST /api/v1/webhooks/conversion` to FunnelSwift (sender state: Rule 4)
- Affiliates handler is local CRUD

### 7. ADA Swift — ADA Compliance Scanning
- **Service under SwiftImpact Solutions** (not a standalone SaaS)
- **Plan-sync sender**: `src/handlers/plans_handler.rs` — does not authenticate today (Rule 4)
- Scans are free, affiliates get paid on plan upgrades only
- The catalogue's only ADASwift entry is `ADASwift Free`; there is no `ADASwift Monthly Scan` product
  row (absent, not merely inactive — measured 2026-09-26)

## Cross-App Flow: Affiliate Signup → Commission

```
1. Affiliate signs up on FunnelSwift → gets affiliate code (AFF-XXXX)
2. Affiliate creates Kinetic card → gets /k/:slug with ?src= param tracking
3. Lead finds card → submits info on the card
   → Auto-created account on FunnelSwift (kinetic_free plan)
   → Tagged with affiliate's tag
   → affiliate_conversions row (pending, $5.00)
4. Lead upgrades to paid plan on FunnelSwift
   → Commission calculated by plan price × commission_rate
   → affiliate_conversions updated to "approved"
5. Affiliate sees earnings in FunnelSwift dashboard
```

## Anti-Rules (Never Do These)

- ❌ Create a separate affiliate module in any app other than FunnelSwift
- ❌ Add `affiliate_code` processing to non-FunnelSwift signup handlers
- ❌ Build parallel sub-agents for builds (serial only)
- ❌ Use `#[allow(...)]` to silence compiler warnings — fix the code
- ❌ Use `.unwrap()` or `.expect()` in production code
- ❌ Merge Zaarcash (loyalty) with Affiliate (commission) — they are separate

## Deployment Sequence

1. Run `cargo check` → fix errors
2. Run `cargo test` → fix failures
3. Run `cargo clippy -- -D warnings` → fix warnings
4. `cargo build --release` (CARGO_BUILD_JOBS=1)
5. `cp target/release/{binary} /opt/swift/{app}/{binary}`
6. `systemctl restart {service}.service`
7. `curl -s localhost:{port}/api/health` → verify 200

## Git
- All repos use `origin` remote
- CoreSwift uses `master` branch, all others use `main`
- Never force push
- Use `/opt/swift/sync-to-repo.sh` for bulk VPS→GitHub sync
